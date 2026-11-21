// Derived from draw.io js/grapheditor/Shapes.js and shapes/mxBasic.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! Each shape's connection points: where an edge end can be pinned, as the shape faces east
//! (the shapes' `constraints` and `getConstraints`, Shapes.js 9025-9600).

use super::basic::{band, iso_height, snip, stripe, within};
use crate::geom::Point;
use crate::model::Page;
use crate::route::Constraint;
use crate::style::Resolved;

/// On the outline at (`x`, `y`) of the bounds.
const fn on(x: f64, y: f64) -> Constraint {
    Constraint {
        point: Point::new(x, y),
        dx: 0.0,
        dy: 0.0,
        perimeter: true,
    }
}

/// Exactly at (`x`, `y`) of the bounds, then `dx`, `dy` further in page units.
const fn at(x: f64, y: f64, dx: f64, dy: f64) -> Constraint {
    Constraint {
        point: Point::new(x, y),
        dx,
        dy,
        perimeter: false,
    }
}

/// Exactly at (`x`, `y`) of the bounds.
const fn fixed(x: f64, y: f64) -> Constraint {
    at(x, y, 0.0, 0.0)
}

/// Connection points for an east-facing `shape` of size `w` by `h` on `page`. A stencil has its
/// own (`<connections>`), none where it names none; other shapes without a table use rectangle
/// points.
pub fn constraints(shape: &str, style: &Resolved, w: f64, h: f64, page: &Page) -> Vec<Constraint> {
    if let Some(stencil) = super::Stencil::of(shape, page) {
        return stencil.constraints().to_vec();
    }
    let table: &[Constraint] = match shape {
        "ellipse" | "doubleEllipse" | "rhombus" | "startState" | "endState" => &ELLIPSE,
        "cylinder" | "message" | "waypoint" => &CYLINDER,
        "component" => &COMPONENT,
        "actor" | "curlyBracket" => &ACTOR,
        "umlActor" => &UML_ACTOR,
        "tape" => &TAPE,
        "step" => &STEP,
        "line" => &LINE,
        "triangle" => &TRIANGLE,
        "hexagon" => &HEXAGON,
        "cloud" => &CLOUD,
        "document" => &DOCUMENT,
        "or" => &OR,
        "xor" => &XOR,
        "note" => return note(style, w, h),
        "card" => return card(style, w, h),
        "cube" => return cube(style, w, h),
        "cylinder3" => return cylinder3(style, w, h),
        "callout" => return callout(style, w, h),
        "folder" => return folder(style, w, h),
        "mxgraph.basic.diag_round_rect" => &DIAG_ROUND_RECT,
        "mxgraph.basic.corner_round_rect" => &CORNER_ROUND_RECT,
        "mxgraph.basic.three_corner_round_rect" => &THREE_CORNER_ROUND_RECT,
        "mxgraph.basic.plaque" => &SIDE_MIDDLES,
        // Neither `constraints` nor `getConstraints`: an edge only meets the outline.
        "mxgraph.basic.cross2" | "mxgraph.basic.drop" | "mxgraph.basic.donut" => &[],
        "mxgraph.basic.diag_snip_rect" => return diag_snip_rect(style, w, h),
        "mxgraph.basic.frame" => return frame(style, w, h),
        "mxgraph.basic.rounded_frame" | "mxgraph.basic.plaque_frame" => {
            return inner_frame(style, w, h);
        }
        "mxgraph.basic.frame_corner" => return frame_corner(style, w, h),
        "mxgraph.basic.diag_stripe" => return diag_stripe(style, w, h),
        "mxgraph.basic.layered_rect" => return layered_rect(style, w, h),
        "mxgraph.basic.octagon2" => return octagon(style, w, h),
        "mxgraph.basic.acute_triangle" => return acute_triangle(style, w, h),
        "mxgraph.basic.obtuse_triangle" => return obtuse_triangle(style, w, h),
        "mxgraph.basic.4_point_star_2" => return four_point_star(style, w),
        "mxgraph.basic.wave2" => return wave(style, w, h),
        "mxgraph.basic.isocube" => return isocube(style, w, h),
        "mxgraph.basic.cone2" => return cone(style, w, h),
        "mxgraph.basic.pyramid" => return pyramid(style, w, h),
        _ => &RECTANGLE,
    };
    table.to_vec()
}

/// `mxRectangleShape.prototype.constraints`: the corners and the quarters of each side.
// Shapes.js 9092-9107
const RECTANGLE: [Constraint; 16] = [
    on(0.0, 0.0),
    on(0.25, 0.0),
    on(0.5, 0.0),
    on(0.75, 0.0),
    on(1.0, 0.0),
    on(0.0, 0.25),
    on(0.0, 0.5),
    on(0.0, 0.75),
    on(1.0, 0.25),
    on(1.0, 0.5),
    on(1.0, 0.75),
    on(0.0, 1.0),
    on(0.25, 1.0),
    on(0.5, 1.0),
    on(0.75, 1.0),
    on(1.0, 1.0),
];

/// `mxEllipse.prototype.constraints`: the corners, which land on the outline, and the middle of
/// each side.
// Shapes.js 9108-9111
const ELLIPSE: [Constraint; 8] = [
    on(0.0, 0.0),
    on(1.0, 0.0),
    on(0.0, 1.0),
    on(1.0, 1.0),
    on(0.5, 0.0),
    on(0.5, 1.0),
    on(0.0, 0.5),
    on(1.0, 0.5),
];

/// `mxShapeBasicDiagRoundRect.getConstraints`: the middle of each side and the two sharp
/// corners (mxBasic.js 1306-1317).
const DIAG_ROUND_RECT: [Constraint; 6] = [
    fixed(0.5, 0.0),
    fixed(1.0, 0.0),
    fixed(1.0, 0.5),
    fixed(0.5, 1.0),
    fixed(0.0, 1.0),
    fixed(0.0, 0.5),
];

/// `mxShapeBasicCornerRoundRect.getConstraints`: the middle of each side and the three sharp
/// corners (mxBasic.js 1390-1403).
const CORNER_ROUND_RECT: [Constraint; 7] = [
    fixed(0.5, 0.0),
    fixed(1.0, 0.0),
    fixed(1.0, 0.5),
    fixed(1.0, 1.0),
    fixed(0.5, 1.0),
    fixed(0.0, 1.0),
    fixed(0.0, 0.5),
];

/// `mxShapeBasicThreeCornerRoundRect.getConstraints`: the middle of each side and the sharp
/// corner (mxBasic.js 3246-3257).
const THREE_CORNER_ROUND_RECT: [Constraint; 5] = [
    fixed(0.5, 0.0),
    fixed(1.0, 0.5),
    fixed(0.5, 1.0),
    fixed(0.0, 1.0),
    fixed(0.0, 0.5),
];

/// The middle of each side: a plaque's (mxBasic.js 1477-1487), and the first of a rounded or
/// plaque frame's.
const SIDE_MIDDLES: [Constraint; 4] = [
    fixed(0.5, 0.0),
    fixed(1.0, 0.5),
    fixed(0.5, 1.0),
    fixed(0.0, 0.5),
];

// Shapes.js 9328-9339
const CYLINDER: [Constraint; 12] = [
    fixed(0.15, 0.05),
    on(0.5, 0.0),
    fixed(0.85, 0.05),
    on(0.0, 0.3),
    on(0.0, 0.5),
    on(0.0, 0.7),
    on(1.0, 0.3),
    on(1.0, 0.5),
    on(1.0, 0.7),
    fixed(0.15, 0.95),
    on(0.5, 1.0),
    fixed(0.85, 0.95),
];

// Shapes.js 9340-9347
const UML_ACTOR: [Constraint; 8] = [
    fixed(0.25, 0.1),
    fixed(0.5, 0.0),
    fixed(0.75, 0.1),
    fixed(0.0, 1.0 / 3.0),
    fixed(0.0, 1.0),
    fixed(1.0, 1.0 / 3.0),
    fixed(1.0, 1.0),
    fixed(0.5, 0.5),
];

// Shapes.js 9348-9358
const COMPONENT: [Constraint; 11] = [
    on(0.25, 0.0),
    on(0.5, 0.0),
    on(0.75, 0.0),
    on(0.0, 0.3),
    on(0.0, 0.7),
    on(1.0, 0.25),
    on(1.0, 0.5),
    on(1.0, 0.75),
    on(0.25, 1.0),
    on(0.5, 1.0),
    on(0.75, 1.0),
];

// Shapes.js 9359-9368
const ACTOR: [Constraint; 10] = [
    on(0.5, 0.0),
    fixed(0.25, 0.2),
    fixed(0.1, 0.5),
    on(0.0, 0.75),
    fixed(0.75, 0.25),
    fixed(0.9, 0.5),
    on(1.0, 0.75),
    on(0.25, 1.0),
    on(0.5, 1.0),
    on(0.75, 1.0),
];

// Shapes.js 9377-9384
const TAPE: [Constraint; 8] = [
    fixed(0.0, 0.35),
    fixed(0.0, 0.5),
    fixed(0.0, 0.65),
    fixed(1.0, 0.35),
    fixed(1.0, 0.5),
    fixed(1.0, 0.65),
    fixed(0.25, 1.0),
    fixed(0.75, 0.0),
];

// Shapes.js 9385-9396
const STEP: [Constraint; 12] = [
    on(0.25, 0.0),
    on(0.5, 0.0),
    on(0.75, 0.0),
    on(0.25, 1.0),
    on(0.5, 1.0),
    on(0.75, 1.0),
    on(0.0, 0.25),
    on(0.0, 0.5),
    on(0.0, 0.75),
    on(1.0, 0.25),
    on(1.0, 0.5),
    on(1.0, 0.75),
];

// Shapes.js 9397-9400
const LINE: [Constraint; 4] = [
    fixed(0.0, 0.5),
    fixed(0.25, 0.5),
    fixed(0.75, 0.5),
    fixed(1.0, 0.5),
];

// Shapes.js 9405-9410
const TRIANGLE: [Constraint; 6] = [
    on(0.0, 0.25),
    on(0.0, 0.5),
    on(0.0, 0.75),
    on(0.5, 0.0),
    on(0.5, 1.0),
    on(1.0, 0.5),
];

/// `mxHexagon`'s, which draw.io's `hexagon` keeps.
// Shapes.js 9411-9422
const HEXAGON: [Constraint; 12] = [
    on(0.375, 0.0),
    on(0.5, 0.0),
    on(0.625, 0.0),
    on(0.0, 0.25),
    on(0.0, 0.5),
    on(0.0, 0.75),
    on(1.0, 0.25),
    on(1.0, 0.5),
    on(1.0, 0.75),
    on(0.375, 1.0),
    on(0.5, 1.0),
    on(0.625, 1.0),
];

// Shapes.js 9423-9434
const CLOUD: [Constraint; 12] = [
    fixed(0.25, 0.25),
    fixed(0.4, 0.1),
    fixed(0.16, 0.55),
    fixed(0.07, 0.4),
    fixed(0.31, 0.8),
    fixed(0.13, 0.77),
    fixed(0.8, 0.8),
    fixed(0.55, 0.95),
    fixed(0.875, 0.5),
    fixed(0.96, 0.7),
    fixed(0.625, 0.2),
    fixed(0.88, 0.25),
];

// Shapes.js 9437-9445
const DOCUMENT: [Constraint; 9] = [
    on(0.25, 0.0),
    on(0.5, 0.0),
    on(0.75, 0.0),
    on(0.0, 0.25),
    on(0.0, 0.5),
    on(0.0, 0.75),
    on(1.0, 0.25),
    on(1.0, 0.5),
    on(1.0, 0.75),
];

// Shapes.js 9586-9591
const OR: [Constraint; 6] = [
    fixed(0.0, 0.25),
    fixed(0.0, 0.5),
    fixed(0.0, 0.75),
    fixed(1.0, 0.5),
    fixed(0.7, 0.1),
    fixed(0.7, 0.9),
];

// Shapes.js 9592-9597
const XOR: [Constraint; 6] = [
    fixed(0.175, 0.25),
    fixed(0.25, 0.5),
    fixed(0.175, 0.75),
    fixed(1.0, 0.5),
    fixed(0.7, 0.1),
    fixed(0.7, 0.9),
];

/// The fold's size of a `note`, `card` or `cube`: `size` within the shape.
fn fold(style: &Resolved, default: f64, w: f64, h: f64) -> f64 {
    style.num("size", default).min(w.min(h)).max(0.0)
}

// Shapes.js 9123-9145
fn note(style: &Resolved, w: f64, h: f64) -> Vec<Constraint> {
    let s = fold(style, 30.0, w, h);
    let mut c = vec![
        fixed(0.0, 0.0),
        at(0.0, 0.0, (w - s) * 0.5, 0.0),
        at(0.0, 0.0, w - s, 0.0),
        at(0.0, 0.0, w - s * 0.5, s * 0.5),
        at(0.0, 0.0, w, s),
        at(0.0, 0.0, w, (h + s) * 0.5),
        fixed(1.0, 1.0),
        fixed(0.5, 1.0),
        fixed(0.0, 1.0),
        fixed(0.0, 0.5),
    ];
    if w >= s * 2.0 {
        c.push(fixed(0.5, 0.0));
    }
    c
}

// Shapes.js 9147-9169
fn card(style: &Resolved, w: f64, h: f64) -> Vec<Constraint> {
    let s = fold(style, 30.0, w, h);
    let mut c = vec![
        fixed(1.0, 0.0),
        at(0.0, 0.0, (w + s) * 0.5, 0.0),
        at(0.0, 0.0, s, 0.0),
        at(0.0, 0.0, s * 0.5, s * 0.5),
        at(0.0, 0.0, 0.0, s),
        at(0.0, 0.0, 0.0, (h + s) * 0.5),
        fixed(0.0, 1.0),
        fixed(0.5, 1.0),
        fixed(1.0, 1.0),
        fixed(1.0, 0.5),
    ];
    if w >= s * 2.0 {
        c.push(fixed(0.5, 0.0));
    }
    c
}

// Shapes.js 9171-9190
fn cube(style: &Resolved, w: f64, h: f64) -> Vec<Constraint> {
    let s = fold(style, 20.0, w, h);
    vec![
        fixed(0.0, 0.0),
        at(0.0, 0.0, (w - s) * 0.5, 0.0),
        at(0.0, 0.0, w - s, 0.0),
        at(0.0, 0.0, w - s * 0.5, s * 0.5),
        at(0.0, 0.0, w, s),
        at(0.0, 0.0, w, (h + s) * 0.5),
        fixed(1.0, 1.0),
        at(0.0, 0.0, (w + s) * 0.5, h),
        at(0.0, 0.0, s, h),
        at(0.0, 0.0, s * 0.5, h - s * 0.5),
        at(0.0, 0.0, 0.0, h - s),
        at(0.0, 0.0, 0.0, (h - s) * 0.5),
    ]
}

// Shapes.js 9192-9218
fn cylinder3(style: &Resolved, _w: f64, h: f64) -> Vec<Constraint> {
    let s = style.num("size", 15.0).min(h).max(0.0);
    let quarter = s + (h * 0.5 - s) * 0.5;
    vec![
        fixed(0.5, 0.0),
        fixed(0.0, 0.5),
        fixed(0.5, 1.0),
        fixed(1.0, 0.5),
        at(0.0, 0.0, 0.0, s),
        at(1.0, 0.0, 0.0, s),
        at(1.0, 1.0, 0.0, -s),
        at(0.0, 1.0, 0.0, -s),
        at(0.0, 0.0, 0.0, quarter),
        at(1.0, 0.0, 0.0, quarter),
        at(1.0, 0.0, 0.0, h - quarter),
        at(0.0, 0.0, 0.0, h - quarter),
        at(0.145, 0.0, 0.0, s * 0.29),
        at(0.855, 0.0, 0.0, s * 0.29),
        at(0.855, 1.0, 0.0, -s * 0.29),
        at(0.145, 1.0, 0.0, -s * 0.29),
    ]
}

// Shapes.js 9064-9090
fn callout(style: &Resolved, w: f64, h: f64) -> Vec<Constraint> {
    let s = style.num("size", 30.0).min(h).max(0.0);
    let dx2 = w * style.num("position2", 0.5).clamp(0.0, 1.0);
    let mut c = vec![
        fixed(0.0, 0.0),
        fixed(0.25, 0.0),
        fixed(0.5, 0.0),
        fixed(0.75, 0.0),
        fixed(1.0, 0.0),
        at(0.0, 0.0, w, (h - s) * 0.5),
        at(0.0, 0.0, w, h - s),
        at(0.0, 0.0, dx2, h),
        at(0.0, 0.0, 0.0, h - s),
        at(0.0, 0.0, 0.0, (h - s) * 0.5),
    ];
    if w >= s * 2.0 {
        c.push(fixed(0.5, 0.0));
    }
    c
}

// Shapes.js 9220-9259
fn folder(style: &Resolved, w: f64, h: f64) -> Vec<Constraint> {
    let dx = style.num("tabWidth", 60.0).min(w).max(0.0);
    let dy = style.num("tabHeight", 20.0).min(h).max(0.0);
    let mut c = match style.get("tabPosition") {
        Some("left") => vec![
            fixed(0.0, 0.0),
            at(0.0, 0.0, dx * 0.5, 0.0),
            at(0.0, 0.0, dx, 0.0),
            at(0.0, 0.0, dx, dy),
            at(0.0, 0.0, (w + dx) * 0.5, dy),
        ],
        _ => vec![
            fixed(1.0, 0.0),
            at(0.0, 0.0, w - dx * 0.5, 0.0),
            at(0.0, 0.0, w - dx, 0.0),
            at(0.0, 0.0, w - dx, dy),
            at(0.0, 0.0, (w - dx) * 0.5, dy),
        ],
    };
    for x in [w, 0.0] {
        c.extend([0.0, 0.25, 0.5, 0.75].map(|f| at(0.0, 0.0, x, (h - dy) * f + dy)));
        c.push(at(0.0, 0.0, x, h));
    }
    c.extend([fixed(0.25, 1.0), fixed(0.5, 1.0), fixed(0.75, 1.0)]);
    c
}

// mxBasic.js 1216-1233
fn diag_snip_rect(style: &Resolved, w: f64, h: f64) -> Vec<Constraint> {
    let d = snip(style, w, h) * 0.5;
    vec![
        at(0.0, 0.0, d, d),
        fixed(0.5, 0.0),
        fixed(1.0, 0.0),
        fixed(1.0, 0.5),
        at(0.0, 0.0, w - d, h - d),
        fixed(0.5, 1.0),
        fixed(0.0, 1.0),
        fixed(0.0, 0.5),
    ]
}

/// A frame's corners and quarters of each side, outside and in.
// mxBasic.js 1562-1602
fn frame(style: &Resolved, w: f64, h: f64) -> Vec<Constraint> {
    let d = band(style, w, h, 0.5);
    vec![
        fixed(0.0, 0.0),
        at(0.0, 0.0, w * 0.25, 0.0),
        fixed(0.5, 0.0),
        at(0.0, 0.0, w * 0.75, 0.0),
        fixed(1.0, 0.0),
        at(0.0, 0.0, w, h * 0.25),
        fixed(1.0, 0.5),
        at(0.0, 0.0, w, h * 0.75),
        fixed(1.0, 1.0),
        at(0.0, 0.0, w * 0.75, h),
        fixed(0.5, 1.0),
        at(0.0, 0.0, w * 0.25, h),
        fixed(0.0, 1.0),
        at(0.0, 0.0, 0.0, h * 0.75),
        fixed(0.0, 0.5),
        at(0.0, 0.0, 0.0, h * 0.25),
        at(0.0, 0.0, d, d),
        at(0.0, 0.0, (w - 2.0 * d) * 0.25 + d, d),
        at(0.5, 0.0, 0.0, d),
        at(0.0, 0.0, (w * 1.5 - d) * 0.5, d),
        at(1.0, 0.0, -d, d),
        at(0.0, 0.0, w - d, (h - 2.0 * d) * 0.25 + d),
        at(1.0, 0.5, -d, 0.0),
        at(0.0, 0.0, w - d, (h - 2.0 * d) * 0.75 + d),
        at(1.0, 1.0, -d, -d),
        at(0.0, 0.0, (w - 2.0 * d) * 0.75 + d, h - d),
        at(0.5, 1.0, 0.0, -d),
        at(0.0, 0.0, (w - 2.0 * d) * 0.25 + d, h - d),
        at(0.0, 1.0, d, -d),
        at(0.0, 0.0, d, (h - 2.0 * d) * 0.75 + d),
        at(0.0, 0.5, d, 0.0),
        at(0.0, 0.0, d, (h - 2.0 * d) * 0.25 + d),
    ]
}

/// A rounded or plaque frame's: the middle of each side, outside and in, the band at most half
/// the shorter side here where the shape draws it a quarter.
// mxBasic.js 1687-1703 and 1789-1805
fn inner_frame(style: &Resolved, w: f64, h: f64) -> Vec<Constraint> {
    let d = band(style, w, h, 0.5);
    let mut c = SIDE_MIDDLES.to_vec();
    c.extend([
        at(0.5, 0.0, 0.0, d),
        at(1.0, 0.5, -d, 0.0),
        at(0.5, 1.0, 0.0, -d),
        at(0.0, 0.5, d, 0.0),
    ]);
    c
}

// mxBasic.js 1877-1896
fn frame_corner(style: &Resolved, w: f64, h: f64) -> Vec<Constraint> {
    let d = band(style, w, h, 0.5);
    vec![
        fixed(0.0, 0.0),
        fixed(0.5, 0.0),
        fixed(1.0, 0.0),
        at(1.0, 0.0, -d, d),
        at(0.0, 0.0, (w - 2.0 * d) * 0.5 + d, d),
        at(0.0, 0.0, d, d),
        at(0.0, 0.0, d, (h - 2.0 * d) * 0.5 + d),
        at(0.0, 1.0, d, -d),
        fixed(0.0, 1.0),
        fixed(0.0, 0.5),
    ]
}

// mxBasic.js 1966-1982
fn diag_stripe(style: &Resolved, w: f64, h: f64) -> Vec<Constraint> {
    let (x, y) = stripe(style, w, h);
    vec![
        fixed(0.0, 1.0),
        fixed(0.5, 0.5),
        fixed(1.0, 0.0),
        at(0.0, 0.0, w, y * 0.5),
        at(0.0, 0.0, w, y),
        at(0.0, 0.0, (w + x) * 0.5, (y + h) * 0.5),
        at(0.0, 0.0, x, h),
        at(0.0, 0.0, x * 0.5, h),
    ]
}

/// A layered rectangle's: along the front box's top and left side and the back box's right side
/// and bottom, and at the middle box's two free corners.
// mxBasic.js 2166-2194
fn layered_rect(style: &Resolved, w: f64, h: f64) -> Vec<Constraint> {
    let d = band(style, w, h, 0.5);
    let mut c: Vec<Constraint> = [0.0, 0.25, 0.5, 0.75, 1.0]
        .map(|f| at(0.0, 0.0, (w - d) * f, 0.0))
        .to_vec();
    c.push(at(0.0, 0.0, w - d * 0.5, d * 0.5));
    c.extend([0.0, 0.25, 0.5, 0.75, 1.0].map(|f| at(0.0, 0.0, w, (h - d) * f + d)));
    c.extend([0.75, 0.5, 0.25, 0.0].map(|f| at(0.0, 0.0, (w - d) * f + d, h)));
    c.push(at(0.0, 0.0, d * 0.5, h - d * 0.5));
    c.extend([1.0, 0.75, 0.5, 0.25].map(|f| at(0.0, 0.0, 0.0, (h - d) * f)));
    c
}

// mxBasic.js 481-498
fn octagon(style: &Resolved, w: f64, h: f64) -> Vec<Constraint> {
    let d = snip(style, w, h) * 0.5;
    vec![
        fixed(0.5, 0.0),
        fixed(0.5, 1.0),
        fixed(0.0, 0.5),
        fixed(1.0, 0.5),
        at(0.0, 0.0, d, d),
        at(0.0, 0.0, w - d, d),
        at(0.0, 0.0, w - d, h - d),
        at(0.0, 0.0, d, h - d),
    ]
}

// mxBasic.js 663-676
fn acute_triangle(style: &Resolved, w: f64, h: f64) -> Vec<Constraint> {
    let dx = w * within(style, "dx", 0.5, w);
    vec![
        fixed(1.0, 1.0),
        fixed(0.5, 1.0),
        fixed(0.0, 1.0),
        at(0.0, 0.0, dx, 0.0),
        at(0.0, 0.0, dx * 0.5, h * 0.5),
        at(0.0, 0.0, w - (w - dx) * 0.5, h * 0.5),
    ]
}

// mxBasic.js 743-756
fn obtuse_triangle(style: &Resolved, w: f64, h: f64) -> Vec<Constraint> {
    let dx = w * within(style, "dx", 0.5, w);
    vec![
        fixed(0.0, 0.0),
        fixed(1.0, 1.0),
        at(0.0, 0.0, w * 0.5, h * 0.5),
        at(0.0, 0.0, (w + dx) * 0.5, h),
        at(0.0, 0.0, dx, h),
        at(0.0, 0.0, dx * 0.5, h * 0.5),
    ]
}

// mxBasic.js 1129-1144
fn four_point_star(style: &Resolved, w: f64) -> Vec<Constraint> {
    let d = 0.5 * within(style, "dx", 0.8, w);
    vec![
        fixed(0.5, 0.0),
        fixed(0.5, 1.0),
        fixed(0.0, 0.5),
        fixed(1.0, 0.5),
        fixed(d, d),
        fixed(1.0 - d, d),
        fixed(1.0 - d, 1.0 - d),
        fixed(d, 1.0 - d),
    ]
}

/// Along a wave's top and bottom, and the middle of its sides.
// mxBasic.js 383-407
fn wave(style: &Resolved, w: f64, h: f64) -> Vec<Constraint> {
    let dy = h * within(style, "dy", 0.5, h);
    vec![
        at(0.0, 0.0, 0.0, dy * 0.5),
        at(0.0, 0.0, w / 6.0, h * 0.015),
        at(0.0, 0.0, w / 3.0, dy * 0.5),
        at(0.0, 0.0, w * 0.5, dy * 0.95),
        at(0.0, 0.0, w * 0.67, dy * 0.5),
        at(0.0, 0.0, w * 0.83, h * 0.015),
        at(0.0, 0.0, w, dy * 0.5),
        at(0.0, 0.0, w, h * 0.5),
        at(0.0, 0.0, w, h - dy * 0.5),
        at(0.0, 0.0, w * 0.83, h - dy * 0.95),
        at(0.0, 0.0, w * 0.67, h - dy * 0.5),
        at(0.0, 0.0, w * 0.5, h - dy * 0.04),
        at(0.0, 0.0, w / 3.0, h - dy * 0.5),
        at(0.0, 0.0, w / 6.0, h - dy * 0.95),
        at(0.0, 0.0, 0.0, h - dy * 0.5),
        at(0.0, 0.0, 0.0, h * 0.5),
    ]
}

// mxBasic.js 580-596
fn isocube(style: &Resolved, w: f64, h: f64) -> Vec<Constraint> {
    let iso = iso_height(style, w, h);
    vec![
        fixed(0.5, 0.0),
        at(0.0, 0.0, w, iso),
        fixed(1.0, 0.5),
        at(0.0, 0.0, w, h - iso),
        fixed(0.5, 1.0),
        at(0.0, 0.0, 0.0, h - iso),
        fixed(0.0, 0.5),
        at(0.0, 0.0, 0.0, iso),
    ]
}

// mxBasic.js 906-919
fn cone(style: &Resolved, w: f64, h: f64) -> Vec<Constraint> {
    let dx = w * within(style, "dx", 0.5, w);
    let dy = h * within(style, "dy", 0.9, h);
    vec![
        at(0.0, 0.0, dx, 0.0),
        at(0.0, 0.0, w, dy),
        at(0.0, 0.0, w * 0.5, h),
        at(0.0, 0.0, 0.0, dy),
    ]
}

// mxBasic.js 1039-1057
fn pyramid(style: &Resolved, w: f64, h: f64) -> Vec<Constraint> {
    let dx1 = w * within(style, "dx1", 0.5, w);
    let dx2 = w * within(style, "dx2", 0.6, w);
    let dy1 = h * within(style, "dy1", 0.9, h);
    let dy2 = h * within(style, "dy2", 0.8, h);
    vec![
        at(0.0, 0.0, dx1, 0.0),
        at(0.0, 0.0, (w + dx1) * 0.5, dy2 * 0.5),
        at(0.0, 0.0, w, dy2),
        at(0.0, 0.0, (w + dx2) * 0.5, (h + dy2) * 0.5),
        at(0.0, 0.0, dx2, h),
        at(0.0, 0.0, dx2 * 0.5, (h + dy1) * 0.5),
        at(0.0, 0.0, 0.0, dy1),
        at(0.0, 0.0, dx1 * 0.5, dy1 * 0.5),
    ]
}
