// Derived from draw.io js/grapheditor/Shapes.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! Each shape's connection points: where an edge end can be pinned, as the shape faces east
//! (the shapes' `constraints` and `getConstraints`, Shapes.js 9025-9600).

use crate::geom::Point;
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

/// The connection points of vertex `shape`, `w` by `h` as it faces east. A shape of no table of
/// its own, a stencil among them, takes the rectangle's.
// ponytail: a stencil's own points (`<connections>`) need the stencil; the rectangle's stand in.
pub fn constraints(shape: &str, style: &Resolved, w: f64, h: f64) -> Vec<Constraint> {
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
