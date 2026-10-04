// Derived from draw.io mxgraph/src/shape/mxMarker.js, js/grapheditor/Shapes.js, shapes/er/mxER.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! Arrow heads on the ends of an edge.

use crate::geom::{PathCmd, Point, Rect};
use crate::shapes;
use crate::style::Color;

/// `mxConstants.DEFAULT_MARKERSIZE`: an arrow head's size when the style gives none.
pub(crate) const DEFAULT_MARKERSIZE: f64 = 6.0;

/// One outline of an arrow head, to be stroked like its edge (never dashed) and filled with
/// `fill`, if any.
#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    pub path: Vec<PathCmd>,
    pub fill: Option<Color>,
}

/// The head `kind` (`classic`, `block`, `open`, `oval`, `diamond`, `ERmany`, …) at `end`,
/// pointing along `unit` (the unit vector from the previous point towards `end`), at the
/// `source` end or the target's: its outlines in paint order. `end` is moved back to where the
/// line should stop so it does not poke through the head; heads drawn across the line leave it
/// where it is. `None`, and `end` left alone, for `none` and any kind not registered.
///
/// `mxMarker.createMarker` with the heads mxMarker.js, Shapes.js and mxER.js register. `fill`
/// is the head's fill colour when it is filled (`endFill`): heads made of lines alone take
/// none, and the `ERzero…` heads fill their circle white.
pub fn marker(
    kind: &str,
    end: &mut Point,
    unit: Point,
    size: f64,
    sw: f64,
    source: bool,
    fill: Option<Color>,
) -> Option<Vec<Marker>> {
    let filled = |path| Marker { path, fill };
    let line = |path| Marker { path, fill: None };
    let heads = match kind {
        "classic" | "classicThin" | "block" | "blockThin" => {
            let notched = kind.starts_with("classic");
            let width_factor = if kind.ends_with("Thin") { 3.0 } else { 2.0 };
            vec![filled(arrow(end, unit, size, sw, notched, width_factor))]
        }
        "open" | "openThin" => {
            let width_factor = if kind == "openThin" { 3.0 } else { 2.0 };
            vec![line(open_arrow(end, unit, size, sw, width_factor))]
        }
        "oval" => vec![filled(oval(end, unit, size))],
        "diamond" | "diamondThin" => vec![filled(diamond(end, unit, size, sw, kind == "diamond"))],
        "halfCircle" => vec![line(half_circle(end, unit, size, sw))],
        "doubleBlock" => vec![filled(double_block(end, unit, size, sw))],
        "box" => vec![filled(square(end, unit, size, sw))],
        "circle" => vec![filled(circle(end, unit, size, sw))],
        "circlePlus" => {
            let pt = *end;
            vec![
                filled(circle(end, unit, size, sw)),
                line(plus(pt, unit, size, sw)),
            ]
        }
        "async" => vec![filled(half_arrow(end, unit, size, sw, source))],
        "openAsync" => vec![line(open_half_arrow(*end, unit, size, sw, source))],
        "mermaidExtension" | "mermaidDiamond" => {
            vec![filled(mermaid(
                end,
                unit,
                size,
                sw,
                kind == "mermaidDiamond",
            ))]
        }
        "manyOptional" | "ERzeroToMany" | "ERzeroToOne" => {
            // `manyOptional` fills its circle like any head, the ER ones white.
            let er = kind.starts_with("ER");
            let ring = match er {
                true => fill.map(|_| Color::WHITE),
                false => fill,
            };
            let (circle, rest) = optional(end, unit, size, sw, kind, er && fill.is_some());
            vec![
                Marker {
                    path: circle,
                    fill: ring,
                },
                line(rest),
            ]
        }
        _ => vec![line(across(*end, unit, size, sw, kind)?)],
    };
    Some(heads)
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

/// `doubleBlock`: two blocks one behind the other, the line stopping at the back of the second
/// (mxMarker.js 183-218).
fn double_block(pe: &mut Point, unit: Point, size: f64, sw: f64) -> Vec<PathCmd> {
    let (end_offset_x, end_offset_y) = (unit.x * sw * 1.118, unit.y * sw * 1.118);
    let (ux, uy) = (unit.x * (size + sw), unit.y * (size + sw));
    let pt = Point::new(pe.x - end_offset_x, pe.y - end_offset_y);
    pe.x += -ux * 2.0 - end_offset_x;
    pe.y += -uy * 2.0 - end_offset_y;
    let p = |x: f64, y: f64| Point::new(pt.x + x, pt.y + y);
    let mut path = closed(&[
        p(0.0, 0.0),
        p(-ux - uy / 2.0, -uy + ux / 2.0),
        p(uy / 2.0 - ux, -uy - ux / 2.0),
    ]);
    path.extend(closed(&[
        p(-ux, -uy),
        p(-2.0 * ux - 0.5 * uy, 0.5 * ux - 2.0 * uy),
        p(-2.0 * ux + 0.5 * uy, -0.5 * ux - 2.0 * uy),
    ]));
    path
}

/// `box`: a square on the end, the line stopping at its back (Shapes.js 7741-7770).
fn square(pe: &mut Point, unit: Point, size: f64, sw: f64) -> Vec<PathCmd> {
    let (nx, ny) = (unit.x * (size + sw + 1.0), unit.y * (size + sw + 1.0));
    let (px, py) = (pe.x + nx / 2.0, pe.y + ny / 2.0);
    pe.x -= nx;
    pe.y -= ny;
    let p = |x: f64, y: f64| Point::new(px + x, py + y);
    closed(&[
        p(-nx / 2.0 - ny / 2.0, -ny / 2.0 + nx / 2.0),
        p(-nx / 2.0 + ny / 2.0, -ny / 2.0 - nx / 2.0),
        p(ny / 2.0 - 3.0 * nx / 2.0, -3.0 * ny / 2.0 - nx / 2.0),
        p(-ny / 2.0 - 3.0 * nx / 2.0, -3.0 * ny / 2.0 + nx / 2.0),
    ])
}

/// `circle`: a circle `size + sw` across behind the end, the line stopping at its back
/// (Shapes.js `circleMarker`, 7788-7814).
fn circle(pe: &mut Point, unit: Point, size: f64, sw: f64) -> Vec<PathCmd> {
    let s = size + sw;
    let pt = *pe;
    pe.x -= unit.x * (2.0 * s + sw);
    pe.y -= unit.y * (2.0 * s + sw);
    let (ux, uy) = (unit.x * (s + sw), unit.y * (s + sw));
    shapes::ellipse(Rect::new(pt.x - ux - s, pt.y - uy - s, 2.0 * s, 2.0 * s))
}

/// The cross inside `circlePlus`'s circle, from the end `pt` (Shapes.js 7817-7836).
fn plus(pt: Point, unit: Point, size: f64, sw: f64) -> Vec<PathCmd> {
    let (nx, ny) = (unit.x * (size + 2.0 * sw), unit.y * (size + 2.0 * sw));
    let (ox, oy) = (unit.x * sw, unit.y * sw);
    let p = |x: f64, y: f64| Point::new(pt.x + x, pt.y + y);
    lines(&[
        &[p(-ox, -oy), p(-2.0 * nx + ox, -2.0 * ny + oy)],
        &[
            p(-nx - ny + oy, -ny + nx - ox),
            p(ny - nx - oy, -ny - nx + ox),
        ],
    ])
}

/// `async`: the half of a block on the source's left, or the target's right, of the line
/// (Shapes.js 7857-7902).
fn half_arrow(pe: &mut Point, unit: Point, size: f64, sw: f64, source: bool) -> Vec<PathCmd> {
    let (end_offset_x, end_offset_y) = (unit.x * sw * 1.118, unit.y * sw * 1.118);
    let (ux, uy) = (unit.x * (size + sw), unit.y * (size + sw));
    let pt = Point::new(pe.x - end_offset_x, pe.y - end_offset_y);
    pe.x += -ux - end_offset_x;
    pe.y += -uy - end_offset_y;
    let wing = match source {
        true => Point::new(pt.x - ux - uy / 2.0, pt.y - uy + ux / 2.0),
        false => Point::new(pt.x + uy / 2.0 - ux, pt.y - uy - ux / 2.0),
    };
    closed(&[pt, wing, Point::new(pt.x - ux, pt.y - uy)])
}

/// `openAsync`: one side of an open arrow, on the same side as `async`'s, the line uncut
/// (Shapes.js 7904-7934).
fn open_half_arrow(pt: Point, unit: Point, size: f64, sw: f64, source: bool) -> Vec<PathCmd> {
    let (ux, uy) = (unit.x * (size + sw), unit.y * (size + sw));
    let wing = match source {
        true => Point::new(pt.x - ux - uy / 2.0, pt.y - uy + ux / 2.0),
        false => Point::new(pt.x + uy / 2.0 - ux, pt.y - uy - ux / 2.0),
    };
    lines(&[&[pt, wing]])
}

/// `mermaidExtension`, a triangle, and with `diamond` `mermaidDiamond`: mermaid's class-diagram
/// heads, 17 long by 12 wide at `size` 17, the tip on the end (Shapes.js 7936-8011).
fn mermaid(pe: &mut Point, unit: Point, size: f64, sw: f64, diamond: bool) -> Vec<PathCmd> {
    let wf = 17.0 / 6.0;
    let (ux, uy) = (unit.x * (size + sw), unit.y * (size + sw));
    let pt = *pe;
    pe.x -= ux;
    pe.y -= uy;
    let p = |x: f64, y: f64| Point::new(pt.x + x, pt.y + y);
    match diamond {
        true => closed(&[
            p(0.0, 0.0),
            p(-ux / 2.0 - uy / wf, -uy / 2.0 + ux / wf),
            p(-ux, -uy),
            p(-ux / 2.0 + uy / wf, -uy / 2.0 - ux / wf),
        ]),
        false => closed(&[
            p(0.0, 0.0),
            p(-ux - uy / wf, -uy + ux / wf),
            p(-ux + uy / wf, -uy - ux / wf),
        ]),
    }
}

/// `manyOptional` and the `ERzero…` heads: a circle behind a crow's foot or a bar, and the line
/// cut at the circle's back unless `solid` (an ER head filled, whose white circle covers the
/// line instead), where the ER heads carry a stub of the line on to the end (mxMarker.js
/// 264-289, mxER.js 1309-1400). The circle, then the lines.
fn optional(
    pe: &mut Point,
    unit: Point,
    size: f64,
    sw: f64,
    kind: &str,
    solid: bool,
) -> (Vec<PathCmd>, Vec<PathCmd>) {
    let (nx, ny) = (unit.x * (size + sw + 1.0), unit.y * (size + sw + 1.0));
    let a = size / 2.0;
    let pt = *pe;
    if !solid {
        pe.x -= 2.0 * nx - unit.x * sw / 2.0;
        pe.y -= 2.0 * ny - unit.y * sw / 2.0;
    }
    let p = |x: f64, y: f64| Point::new(pt.x + x, pt.y + y);
    let ring = Rect::new(pt.x - 1.5 * nx - a, pt.y - 1.5 * ny - a, 2.0 * a, 2.0 * a);
    let foot = [p(ny / 2.0, -nx / 2.0), p(-nx, -ny), p(-ny / 2.0, nx / 2.0)];
    let rest = match kind {
        "ERzeroToOne" => {
            let bar = [
                p(-nx / 2.0 - ny / 2.0, -ny / 2.0 + nx / 2.0),
                p(-nx / 2.0 + ny / 2.0, -ny / 2.0 - nx / 2.0),
            ];
            let stub = [p(-nx - unit.x * sw / 2.0, -ny - unit.y * sw / 2.0), pt];
            match solid {
                true => lines(&[&bar]),
                false => lines(&[&bar, &stub]),
            }
        }
        "ERzeroToMany" => match solid {
            true => lines(&[&foot]),
            false => lines(&[&foot, &[p(-nx, -ny), pt]]),
        },
        // `manyOptional`: the line on to the tip of the foot.
        _ => lines(&[&[pt, p(-nx, -ny)], &foot]),
    };
    (shapes::ellipse(ring), rest)
}

/// The heads drawn across the end, which leave the line as it is: `baseDash` (mxMarker.js
/// 164-181), `dash` and `cross` (Shapes.js 7726-7786) and mxER.js's `ERone`, `ERmandOne`,
/// `ERmany` and `ERoneToMany` (1247-1307). `None` for any other kind.
fn across(pe: Point, unit: Point, size: f64, sw: f64, kind: &str) -> Option<Vec<PathCmd>> {
    let (nx, ny) = (unit.x * (size + sw + 1.0), unit.y * (size + sw + 1.0));
    let p = |x: f64, y: f64| Point::new(pe.x + x, pe.y + y);
    // A bar across the line, half the head's length back from the end, and one a length back.
    let bar = [
        p(-nx / 2.0 - ny / 2.0, -ny / 2.0 + nx / 2.0),
        p(-nx / 2.0 + ny / 2.0, -ny / 2.0 - nx / 2.0),
    ];
    let far_bar = [
        p(-nx - ny / 2.0, -ny + nx / 2.0),
        p(-nx + ny / 2.0, -ny - nx / 2.0),
    ];
    let foot = [p(ny / 2.0, -nx / 2.0), p(-nx, -ny), p(-ny / 2.0, nx / 2.0)];
    // The dash and the cross's strokes slant back across the line.
    let slash = [
        p(-nx / 2.0 - ny / 2.0, -ny / 2.0 + nx / 2.0),
        p(ny / 2.0 - 3.0 * nx / 2.0, -3.0 * ny / 2.0 - nx / 2.0),
    ];
    let backslash = [
        p(-nx / 2.0 + ny / 2.0, -ny / 2.0 - nx / 2.0),
        p(-ny / 2.0 - 3.0 * nx / 2.0, -3.0 * ny / 2.0 + nx / 2.0),
    ];
    Some(match kind {
        "baseDash" => lines(&[&[p(-ny / 2.0, nx / 2.0), p(ny / 2.0, -nx / 2.0)]]),
        "dash" => lines(&[&slash]),
        "cross" => lines(&[&slash, &backslash]),
        "ERone" => lines(&[&bar]),
        "ERmandOne" => lines(&[&bar, &far_bar]),
        "ERmany" => lines(&[&foot]),
        "ERoneToMany" => lines(&[&far_bar, &foot]),
        _ => return None,
    })
}

/// Open polylines, each its own stroke.
fn lines(polylines: &[&[Point]]) -> Vec<PathCmd> {
    let mut path = Vec::new();
    for line in polylines {
        if let Some((&first, rest)) = line.split_first() {
            path.push(PathCmd::MoveTo(first));
            path.extend(rest.iter().map(|&p| PathCmd::LineTo(p)));
        }
    }
    path
}

/// A closed polygon through `pts`.
fn closed(pts: &[Point]) -> Vec<PathCmd> {
    let mut path = lines(&[pts]);
    path.push(PathCmd::Close);
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::path_bounds;

    const RIGHT: Point = Point::new(1.0, 0.0);

    /// `kind` at the target end (100, 0) pointing right, size 6 and stroke 1: the head's first
    /// outline and where the line ends.
    fn head(kind: &str, filled: bool) -> (Option<Marker>, Point) {
        let (heads, end) = heads(kind, filled);
        (heads.map(|h| h[0].clone()), end)
    }

    /// Every outline of `kind`, as [`head`] places it.
    fn heads(kind: &str, filled: bool) -> (Option<Vec<Marker>>, Point) {
        let mut end = Point::new(100.0, 0.0);
        let fill = filled.then_some(Color::BLACK);
        let m = marker(kind, &mut end, RIGHT, 6.0, 1.0, false, fill);
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
        assert!(m.fill.is_some());
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
        assert!(m.fill.is_some());
        assert!(head("block", false).0.unwrap().fill.is_none());
    }

    #[test]
    fn open_is_never_filled() {
        let (m, end) = head("open", true);
        let m = m.unwrap();
        assert!(m.fill.is_none());
        assert!(!m.path.contains(&PathCmd::Close));
        assert!(near(end.x, 100.0 - 2.0 * 1.118));
    }

    #[test]
    fn oval_is_a_circle_at_the_end() {
        let (m, end) = head("oval", true);
        let m = m.unwrap();
        assert!(m.fill.is_some());
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
        assert!(m.fill.is_none());
        assert!(!m.path.contains(&PathCmd::Close));
        assert!(near(end.x, 92.0), "size + sw + 1 back: {end:?}");
        assert!(matches!(m.path[1], PathCmd::QuadTo(_, p) if p == end));
    }

    #[test]
    fn heads_across_the_line_leave_it_uncut() {
        for kind in [
            "baseDash",
            "dash",
            "cross",
            "ERone",
            "ERmandOne",
            "ERmany",
            "ERoneToMany",
        ] {
            let (m, end) = head(kind, true);
            let m = m.unwrap();
            assert!(
                m.fill.is_none() && !m.path.contains(&PathCmd::Close),
                "{kind}"
            );
            assert_eq!(end, Point::new(100.0, 0.0), "{kind}");
        }
        // A crow's foot: its toes `size + sw + 1` back from the end.
        let (m, _) = head("ERmany", true);
        let toe = Point::new(92.0, 0.0);
        assert_eq!(m.unwrap().path[1], PathCmd::LineTo(toe));
    }

    #[test]
    fn an_er_zero_head_rings_the_line_in_white_when_filled() {
        let (m, end) = heads("ERzeroToMany", true);
        let m = m.unwrap();
        assert_eq!(m[0].fill, Some(Color::WHITE), "the circle covers the line");
        assert!(m[1].fill.is_none());
        assert_eq!(end, Point::new(100.0, 0.0));
        let (m, end) = heads("ERzeroToMany", false);
        assert!(m.unwrap()[0].fill.is_none());
        // Cut at the circle's back: two head lengths less half the stroke.
        assert!(near(end.x, 100.0 - (2.0 * 8.0 - 0.5)), "{end:?}");
    }

    #[test]
    fn async_takes_one_side_by_the_end_it_is_on() {
        let side = |source: bool| {
            let mut end = Point::new(100.0, 0.0);
            let m = marker("async", &mut end, RIGHT, 6.0, 1.0, source, None).unwrap();
            let wing = match m[0].path[1] {
                PathCmd::LineTo(p) => p,
                ref other => panic!("{other:?}"),
            };
            wing.y
        };
        assert!(side(true) > 0.0 && side(false) < 0.0);
    }

    #[test]
    fn a_circle_sits_behind_the_end() {
        let (m, end) = head("circle", true);
        let b = path_bounds(&m.unwrap().path).unwrap();
        // 7 across both ways, its front a stroke's width back from the end.
        assert!(near(b.right(), 99.0) && near(b.w, 14.0), "{b:?}");
        assert!(near(end.x, 100.0 - 15.0), "{end:?}");
    }

    #[test]
    fn unknown_marker_is_none() {
        for kind in ["none", "", "ERfoo", "arrow"] {
            let (m, end) = head(kind, true);
            assert!(m.is_none(), "{kind}");
            assert_eq!(end, Point::new(100.0, 0.0), "{kind} leaves the end alone");
        }
    }
}
