// Derived from draw.io shapes/mxBasic.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! The shapes of draw.io's Basic palette that it draws in code (`mxBasic.js`). Each reads its
//! handles as the shape's constructor gives them when the style has none, and an `arcTo` that is
//! a quarter turn is drawn with [`quarter`], any other with [`arc_to_curves`].

use super::stencil::arc_to_curves;
use super::{Margins, Part, polyline, quarter};
use crate::geom::{PathCmd, Point, Rect};
use crate::style::Resolved;

/// The style's `key`, or `default` where it has none, kept between 0 and `max`, as each shape
/// reads it (`Math.max(0, Math.min(max, value))`).
pub(super) fn within(style: &Resolved, key: &str, default: f64, max: f64) -> f64 {
    style.num(key, default).min(max).max(0.0)
}

/// The cut or rounding of a snipped, rounded or plaque rectangle's corners and an octagon's:
/// twice `dx`, at most half the shorter side.
pub(super) fn snip(style: &Resolved, w: f64, h: f64) -> f64 {
    (within(style, "dx", 0.5, w) * 2.0)
        .min(w * 0.5)
        .min(h * 0.5)
}

/// A frame's band, a donut's ring or a layered rectangle's step: `dx`, at most `share` of the
/// shorter side.
pub(super) fn band(style: &Resolved, w: f64, h: f64, share: f64) -> f64 {
    within(style, "dx", 0.5, w).min(w * share).min(h * share)
}

/// Where a diagonal stripe's inner edge meets the bottom and the right side.
pub(super) fn stripe(style: &Resolved, w: f64, h: f64) -> (f64, f64) {
    let d = within(style, "dx", 0.5, w).min(h);
    ((d * 100.0 / h).min(w), (d * 100.0 / w).min(h))
}

/// How far an isometric cube's top corners sit below its tip: `isoAngle` in 200ths of a half
/// turn, as draw.io reads it.
pub(super) fn iso_height(style: &Resolved, w: f64, h: f64) -> f64 {
    let angle = style.num("isoAngle", 15.0).clamp(0.01, 94.0) * std::f64::consts::PI / 200.0;
    (w * angle.tan()).min(h * 0.5)
}

/// A closed outline through `pts`, each given from the top-left of `b`.
fn outline(b: Rect, pts: &[(f64, f64)]) -> Vec<PathCmd> {
    let pts: Vec<Point> = pts
        .iter()
        .map(|&(x, y)| Point::new(b.x + x, b.y + y))
        .collect();
    let mut path = polyline(&pts);
    path.push(PathCmd::Close);
    path
}

/// `mxgraph.basic.diag_round_rect`: a box with its top-left and bottom-right corners rounded,
/// twice `dx` across (`mxShapeBasicDiagRoundRect`, mxBasic.js 1241-1285).
pub(super) fn diag_round_rect(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let d = snip(style, w, h);
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    vec![Part::body(vec![
        PathCmd::MoveTo(p(d, 0.0)),
        PathCmd::LineTo(p(w, 0.0)),
        PathCmd::LineTo(p(w, h - d)),
        quarter(p(w, h - d), p(w, h), p(w - d, h)),
        PathCmd::LineTo(p(0.0, h)),
        PathCmd::LineTo(p(0.0, d)),
        quarter(p(0.0, d), p(0.0, 0.0), p(d, 0.0)),
        PathCmd::Close,
    ])]
}

/// `mxgraph.basic.corner_round_rect`: a box with its top-left corner rounded
/// (`mxShapeBasicCornerRoundRect`, mxBasic.js 1326-1369).
pub(super) fn corner_round_rect(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let d = snip(style, w, h);
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    vec![Part::body(vec![
        PathCmd::MoveTo(p(d, 0.0)),
        PathCmd::LineTo(p(w, 0.0)),
        PathCmd::LineTo(p(w, h)),
        PathCmd::LineTo(p(0.0, h)),
        PathCmd::LineTo(p(0.0, d)),
        quarter(p(0.0, d), p(0.0, 0.0), p(d, 0.0)),
        PathCmd::Close,
    ])]
}

/// `mxgraph.basic.three_corner_round_rect`: a box with all but its bottom-left corner rounded
/// (`mxShapeBasicThreeCornerRoundRect`, mxBasic.js 3180-3225).
pub(super) fn three_corner_round_rect(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let d = snip(style, w, h);
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    vec![Part::body(vec![
        PathCmd::MoveTo(p(d, 0.0)),
        PathCmd::LineTo(p(w - d, 0.0)),
        quarter(p(w - d, 0.0), p(w, 0.0), p(w, d)),
        PathCmd::LineTo(p(w, h - d)),
        quarter(p(w, h - d), p(w, h), p(w - d, h)),
        PathCmd::LineTo(p(0.0, h)),
        PathCmd::LineTo(p(0.0, d)),
        quarter(p(0.0, d), p(0.0, 0.0), p(d, 0.0)),
        PathCmd::Close,
    ])]
}

/// `mxgraph.basic.diag_snip_rect`: a box with its top-left and bottom-right corners cut
/// (`mxShapeBasicDiagSnipRect`, mxBasic.js 1152-1195).
pub(super) fn diag_snip_rect(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let d = snip(style, w, h);
    let pts = [
        (d, 0.0),
        (w, 0.0),
        (w, h - d),
        (w - d, h),
        (0.0, h),
        (0.0, d),
    ];
    vec![Part::body(outline(b, &pts))]
}

/// A box `w` by `h` from `p`'s origin with each corner rounded inwards by `d`, clockwise from the
/// top-right corner: a plaque.
fn plaque_outline(p: impl Fn(f64, f64) -> Point, w: f64, h: f64, d: f64) -> Vec<PathCmd> {
    vec![
        PathCmd::MoveTo(p(w - d, 0.0)),
        quarter(p(w - d, 0.0), p(w - d, d), p(w, d)),
        PathCmd::LineTo(p(w, h - d)),
        quarter(p(w, h - d), p(w - d, h - d), p(w - d, h)),
        PathCmd::LineTo(p(d, h)),
        quarter(p(d, h), p(d, h - d), p(0.0, h - d)),
        PathCmd::LineTo(p(0.0, d)),
        quarter(p(0.0, d), p(d, d), p(d, 0.0)),
        PathCmd::Close,
    ]
}

/// `mxgraph.basic.plaque`: a box with its corners rounded inwards
/// (`mxShapeBasicPlaque`, mxBasic.js 1411-1456).
pub(super) fn plaque(b: Rect, style: &Resolved) -> Vec<Part> {
    let d = snip(style, b.w, b.h);
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    vec![Part::body(plaque_outline(p, b.w, b.h, d))]
}

/// `mxgraph.basic.frame`: a box with a hole `dx` in from each side
/// (`mxShapeBasicFrame`, mxBasic.js 1495-1541).
pub(super) fn frame(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let d = band(style, w, h, 0.5);
    let mut path = outline(b, &[(w, 0.0), (w, h), (0.0, h), (0.0, 0.0)]);
    path.extend(outline(
        b,
        &[(d, d), (d, h - d), (w - d, h - d), (w - d, d)],
    ));
    vec![Part::body(path)]
}

/// `mxgraph.basic.rounded_frame`: a rounded box with a rounded hole `dx` in from each side
/// (`mxShapeBasicRoundedFrame`, mxBasic.js 1711-1768).
pub(super) fn rounded_frame(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let d = band(style, w, h, 0.25);
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    vec![Part::body(vec![
        PathCmd::MoveTo(p(w - 2.0 * d, 0.0)),
        quarter(p(w - 2.0 * d, 0.0), p(w, 0.0), p(w, 2.0 * d)),
        PathCmd::LineTo(p(w, h - 2.0 * d)),
        quarter(p(w, h - 2.0 * d), p(w, h), p(w - 2.0 * d, h)),
        PathCmd::LineTo(p(2.0 * d, h)),
        quarter(p(2.0 * d, h), p(0.0, h), p(0.0, h - 2.0 * d)),
        PathCmd::LineTo(p(0.0, 2.0 * d)),
        quarter(p(0.0, 2.0 * d), p(0.0, 0.0), p(2.0 * d, 0.0)),
        PathCmd::Close,
        PathCmd::MoveTo(p(2.0 * d, d)),
        quarter(p(2.0 * d, d), p(d, d), p(d, 2.0 * d)),
        PathCmd::LineTo(p(d, h - 2.0 * d)),
        quarter(p(d, h - 2.0 * d), p(d, h - d), p(2.0 * d, h - d)),
        PathCmd::LineTo(p(w - 2.0 * d, h - d)),
        quarter(
            p(w - 2.0 * d, h - d),
            p(w - d, h - d),
            p(w - d, h - 2.0 * d),
        ),
        PathCmd::LineTo(p(w - d, 2.0 * d)),
        quarter(p(w - d, 2.0 * d), p(w - d, d), p(w - 2.0 * d, d)),
        PathCmd::Close,
    ])]
}

/// `mxgraph.basic.plaque_frame`: a plaque with a plaque-shaped hole `dx` in from each side, its
/// corners arcs twice `dx` round that are shorter than a quarter turn
/// (`mxShapeBasicPlaqueFrame`, mxBasic.js 1610-1666).
pub(super) fn plaque_frame(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let d = band(style, w, h, 0.25);
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    let mut path = plaque_outline(p, w, h, d);
    let arc = |from: Point, to: Point| arc_to_curves(from, 2.0 * d, 2.0 * d, 0.0, false, true, to);
    path.push(PathCmd::MoveTo(p(2.0 * d, d)));
    path.extend(arc(p(2.0 * d, d), p(d, 2.0 * d)));
    path.push(PathCmd::LineTo(p(d, h - 2.0 * d)));
    path.extend(arc(p(d, h - 2.0 * d), p(2.0 * d, h - d)));
    path.push(PathCmd::LineTo(p(w - 2.0 * d, h - d)));
    path.extend(arc(p(w - 2.0 * d, h - d), p(w - d, h - 2.0 * d)));
    path.push(PathCmd::LineTo(p(w - d, 2.0 * d)));
    path.extend(arc(p(w - d, 2.0 * d), p(w - 2.0 * d, d)));
    path.push(PathCmd::Close);
    vec![Part::body(path)]
}

/// `mxgraph.basic.frame_corner`: the top and left sides of a frame, `dx` wide
/// (`mxShapeBasicFrameCorner`, mxBasic.js 1813-1856).
pub(super) fn frame_corner(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let d = band(style, w, h, 0.5);
    let pts = [
        (0.0, 0.0),
        (w, 0.0),
        (w - d, d),
        (d, d),
        (d, h - d),
        (0.0, h),
    ];
    vec![Part::body(outline(b, &pts))]
}

/// `mxgraph.basic.diag_stripe`: a band from the bottom-left corner to the top-right one
/// (`mxShapeBasicDiagStripe`, mxBasic.js 1904-1945).
pub(super) fn diag_stripe(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let (x, y) = stripe(style, w, h);
    vec![Part::body(outline(
        b,
        &[(0.0, h), (w, 0.0), (w, y), (x, h)],
    ))]
}

/// `mxgraph.basic.layered_rect`: three boxes stacked towards the top-left, `dx` apart in all
/// (`mxShapeBasicLayeredRect`, mxBasic.js 2065-2145).
pub(super) fn layered_rect(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let d = band(style, w, h, 0.5);
    let layer = |x: f64, y: f64| {
        let pts = [
            (x, y),
            (w - d + x, y),
            (w - d + x, h - d + y),
            (x, h - d + y),
        ];
        Part::body(outline(b, &pts))
    };
    vec![layer(d, d), layer(d * 0.5, d * 0.5), layer(0.0, 0.0)]
}

/// With `boundedLbl=1` a layered rectangle's label keeps to the front box
/// (`mxShapeBasicLayeredRect.getLabelMargins`, mxBasic.js 2093-2107).
pub(super) fn layered_rect_margins(rect: Rect, style: &Resolved) -> Margins {
    let d = band(style, rect.w, rect.h, 0.5);
    Margins {
        right: d,
        bottom: d,
        ..Margins::default()
    }
}

/// `mxgraph.basic.octagon2`: a box with its corners cut twice `dx` deep
/// (`mxShapeBasicOctagon`, mxBasic.js 415-460).
pub(super) fn octagon(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let d = snip(style, w, h);
    let pts = [
        (d, 0.0),
        (w - d, 0.0),
        (w, d),
        (w, h - d),
        (w - d, h),
        (d, h),
        (0.0, h - d),
        (0.0, d),
    ];
    vec![Part::body(outline(b, &pts))]
}

/// `mxgraph.basic.cross2`: a plus whose arms are twice `dx` thick
/// (`mxShapeBasicCross`, mxBasic.js 10-53).
pub(super) fn cross(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let d = within(style, "dx", 0.5, w);
    let (l, r, t, u) = (w * 0.5 - d, w * 0.5 + d, h * 0.5 - d, h * 0.5 + d);
    let pts = [
        (r, 0.0),
        (r, t),
        (w, t),
        (w, u),
        (r, u),
        (r, h),
        (l, h),
        (l, u),
        (0.0, u),
        (0.0, t),
        (l, t),
        (l, 0.0),
    ];
    vec![Part::body(outline(b, &pts))]
}

/// `mxgraph.basic.acute_triangle`: a triangle on its base, its tip `dx` of the width along
/// (`mxShapeBasicTriangleAcute`, mxBasic.js 604-642).
pub(super) fn acute_triangle(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let dx = w * within(style, "dx", 0.5, w);
    vec![Part::body(outline(b, &[(0.0, h), (dx, 0.0), (w, h)]))]
}

/// `mxgraph.basic.obtuse_triangle`: a triangle from the top-left corner to the bottom-right one
/// and back along the bottom to `dx` of the width (`mxShapeBasicTriangleObtuse`,
/// mxBasic.js 684-722).
pub(super) fn obtuse_triangle(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let dx = w * within(style, "dx", 0.5, w);
    vec![Part::body(outline(b, &[(dx, h), (0.0, 0.0), (w, h)]))]
}

/// `mxgraph.basic.4_point_star_2`: a star pointing at the middle of each side, its inner corners
/// half `dx` of the box in from its corners (`mxShapeBasic4PointStar2`, mxBasic.js 1065-1108).
pub(super) fn four_point_star(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let d = 0.5 * within(style, "dx", 0.8, w);
    let (x, y) = (d * w, d * h);
    let pts = [
        (0.0, h * 0.5),
        (x, y),
        (w * 0.5, 0.0),
        (w - x, y),
        (w, h * 0.5),
        (w - x, h - y),
        (w * 0.5, h),
        (x, h - y),
    ];
    vec![Part::body(outline(b, &pts))]
}

/// `mxgraph.basic.drop`: a circle as wide as the shorter side at the bottom, met by the two lines
/// from the tip at the top that touch it (`mxShapeBasicDrop`, mxBasic.js 764-807).
pub(super) fn drop(b: Rect) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let r = h.min(w) * 0.5;
    let d = h - r;
    let angle = ((d * d - r * r).sqrt() / r).atan();
    let (x1, y1) = (r * angle.sin(), r * angle.cos());
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    let arc = |from: Point, to: Point| arc_to_curves(from, r, r, 0.0, false, true, to);
    let mut path = vec![
        PathCmd::MoveTo(p(w * 0.5, 0.0)),
        PathCmd::LineTo(p(w * 0.5 + x1, h - r - y1)),
    ];
    path.extend(arc(p(w * 0.5 + x1, h - r - y1), p(w * 0.5 + r, h - r)));
    path.push(quarter(
        p(w * 0.5 + r, h - r),
        p(w * 0.5 + r, h),
        p(w * 0.5, h),
    ));
    path.push(quarter(
        p(w * 0.5, h),
        p(w * 0.5 - r, h),
        p(w * 0.5 - r, h - r),
    ));
    path.extend(arc(p(w * 0.5 - r, h - r), p(w * 0.5 - x1, h - r - y1)));
    path.push(PathCmd::Close);
    vec![Part::body(path)]
}

/// `mxgraph.basic.donut`: an ellipse with an elliptical hole, its ring `dx` wide
/// (`mxShapeBasicDonut`, mxBasic.js 1990-2038).
pub(super) fn donut(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let d = band(style, w, h, 0.5);
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    vec![Part::body(vec![
        PathCmd::MoveTo(p(0.0, h * 0.5)),
        quarter(p(0.0, h * 0.5), p(0.0, 0.0), p(w * 0.5, 0.0)),
        quarter(p(w * 0.5, 0.0), p(w, 0.0), p(w, h * 0.5)),
        quarter(p(w, h * 0.5), p(w, h), p(w * 0.5, h)),
        quarter(p(w * 0.5, h), p(0.0, h), p(0.0, h * 0.5)),
        PathCmd::Close,
        PathCmd::MoveTo(p(w * 0.5, d)),
        quarter(p(w * 0.5, d), p(d, d), p(d, h * 0.5)),
        quarter(p(d, h * 0.5), p(d, h - d), p(w * 0.5, h - d)),
        quarter(p(w * 0.5, h - d), p(w - d, h - d), p(w - d, h * 0.5)),
        quarter(p(w - d, h * 0.5), p(w - d, d), p(w * 0.5, d)),
        PathCmd::Close,
    ])]
}

/// `mxgraph.basic.wave2`: a band whose top and bottom are the same wave, `dy` of the height deep
/// (`mxShapeBasicWave`, mxBasic.js 318-362).
pub(super) fn wave(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let dy = h * within(style, "dy", 0.5, h);
    let fy = 1.4;
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    vec![Part::body(vec![
        PathCmd::MoveTo(p(0.0, dy / 2.0)),
        PathCmd::QuadTo(p(w / 6.0, dy * (1.0 - fy)), p(w / 3.0, dy / 2.0)),
        PathCmd::QuadTo(p(w / 2.0, dy * fy), p(w * 2.0 / 3.0, dy / 2.0)),
        PathCmd::QuadTo(p(w * 5.0 / 6.0, dy * (1.0 - fy)), p(w, dy / 2.0)),
        PathCmd::LineTo(p(w, h - dy / 2.0)),
        PathCmd::QuadTo(
            p(w * 5.0 / 6.0, h - dy * fy),
            p(w * 2.0 / 3.0, h - dy / 2.0),
        ),
        PathCmd::QuadTo(p(w / 2.0, h - dy * (1.0 - fy)), p(w / 3.0, h - dy / 2.0)),
        PathCmd::QuadTo(p(w / 6.0, h - dy * fy), p(0.0, h - dy / 2.0)),
        PathCmd::Close,
    ])]
}

/// `mxgraph.basic.isocube`: a cube seen from a corner, its top `isoAngle` steep, with the edges
/// of the front corner stroked over it (`mxShapeBasicIsoCube`, mxBasic.js 506-558).
pub(super) fn isocube(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let iso = iso_height(style, w, h);
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    let pts = [
        (w * 0.5, 0.0),
        (w, iso),
        (w, h - iso),
        (w * 0.5, h),
        (0.0, h - iso),
        (0.0, iso),
    ];
    let edges = vec![
        PathCmd::MoveTo(p(0.0, iso)),
        PathCmd::LineTo(p(w * 0.5, 2.0 * iso)),
        PathCmd::LineTo(p(w, iso)),
        PathCmd::MoveTo(p(w * 0.5, 2.0 * iso)),
        PathCmd::LineTo(p(w * 0.5, h)),
    ];
    vec![Part::body(outline(b, &pts)), Part::line(edges)]
}

/// `mxgraph.basic.cone2`: a cone with its tip `dx` of the width along the top, on a half ellipse
/// from `dy` of the height down to the bottom (`mxShapeBasicCone2`, mxBasic.js 819-873).
pub(super) fn cone(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let dx = w * within(style, "dx", 0.5, w);
    let dy = h * within(style, "dy", 0.9, h);
    let ry = h - dy;
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    let mut path = vec![PathCmd::MoveTo(p(dx, 0.0))];
    if ry > 0.0 {
        path.extend([
            PathCmd::LineTo(p(w, h - ry)),
            quarter(p(w, h - ry), p(w, h), p(w * 0.5, h)),
            quarter(p(w * 0.5, h), p(0.0, h), p(0.0, h - ry)),
        ]);
    } else {
        path.extend([PathCmd::LineTo(p(w, h)), PathCmd::LineTo(p(0.0, h))]);
    }
    path.push(PathCmd::Close);
    vec![Part::body(path)]
}

/// `mxgraph.basic.pyramid`: a pyramid seen from the side, its tip `dx1` of the width along the
/// top, its front corner `dx2` along the bottom and its sides `dy1` and `dy2` of the height down,
/// with its front edge stroked over it (`mxShapeBasicPyramid`, mxBasic.js 927-982).
pub(super) fn pyramid(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let dx1 = w * within(style, "dx1", 0.5, w);
    let dx2 = w * within(style, "dx2", 0.6, w);
    let dy1 = h * within(style, "dy1", 0.9, h);
    let dy2 = h * within(style, "dy2", 0.8, h);
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    let body = outline(b, &[(dx1, 0.0), (w, dy2), (dx2, h), (0.0, dy1)]);
    let edge = vec![PathCmd::MoveTo(p(dx1, 0.0)), PathCmd::LineTo(p(dx2, h))];
    vec![Part::body(body), Part::line(edge)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shapes::tests::{BOX, style};
    use crate::shapes::{is_known, label_bounds, rect, vertex};

    #[test]
    fn a_diagonal_round_rect_rounds_two_corners_by_twice_dx() {
        let b = Rect::new(0.0, 0.0, 100.0, 60.0);
        let ends = |s: &str| -> Vec<Point> {
            diag_round_rect(b, &style(s, false))[0]
                .path
                .iter()
                .filter_map(|c| match c {
                    PathCmd::MoveTo(p) | PathCmd::LineTo(p) | PathCmd::CurveTo(_, _, p) => Some(*p),
                    _ => None,
                })
                .collect()
        };
        let pts = |v: &[(f64, f64)]| v.iter().map(|&(x, y)| Point::new(x, y)).collect::<Vec<_>>();
        assert_eq!(
            ends("shape=mxgraph.basic.diag_round_rect;dx=6;"),
            pts(&[
                (12.0, 0.0),
                (100.0, 0.0),
                (100.0, 48.0),
                (88.0, 60.0),
                (0.0, 60.0),
                (0.0, 12.0),
                (12.0, 0.0)
            ])
        );
        // At most half the shorter side, and 0.5 when the style gives none.
        assert_eq!(ends("dx=100;")[0], Point::new(30.0, 0.0));
        assert_eq!(ends("")[0], Point::new(1.0, 0.0));
    }

    #[test]
    fn every_ported_basic_shape_is_drawn_as_itself() {
        let names = [
            "corner_round_rect",
            "three_corner_round_rect",
            "diag_snip_rect",
            "plaque",
            "frame",
            "rounded_frame",
            "plaque_frame",
            "frame_corner",
            "diag_stripe",
            "layered_rect",
            "octagon2",
            "cross2",
            "acute_triangle",
            "obtuse_triangle",
            "4_point_star_2",
            "drop",
            "donut",
            "wave2",
            "isocube",
            "cone2",
            "pyramid",
        ];
        for name in names.map(|n| format!("mxgraph.basic.{n}")) {
            assert!(is_known(&name), "{name}");
            let parts = vertex(&name, BOX, &style("dx=10;", false));
            assert_ne!(
                parts,
                vec![Part::body(rect(BOX))],
                "{name} is not the stand-in"
            );
        }
    }

    #[test]
    fn sizes_keep_within_the_box_and_default_to_the_constructors() {
        let (w, h) = (100.0, 60.0);
        assert_eq!(snip(&style("dx=100;", false), w, h), 30.0);
        assert_eq!(band(&style("dx=100;", false), w, h, 0.25), 15.0);
        assert_eq!(band(&style("", false), w, h, 0.5), 0.5);
        // `isoAngle` counts 200ths of a half turn, and the top stops at half the height.
        let iso = iso_height(&style("", false), w, w);
        assert!((iso - w * (15.0 * std::f64::consts::PI / 200.0).tan()).abs() < 1e-9);
        assert_eq!(iso_height(&style("isoAngle=94;", false), w, h), 30.0);
    }

    #[test]
    fn a_drop_s_sides_touch_its_circle() {
        let b = Rect::new(0.0, 0.0, 70.0, 100.0);
        let path = &drop(b)[0].path;
        let (tip, centre, r) = (Point::new(35.0, 0.0), Point::new(35.0, 65.0), 35.0);
        let PathCmd::LineTo(side) = path[1] else {
            panic!("a line from the tip, not {:?}", path[1]);
        };
        assert!((side.distance(centre) - r).abs() < 1e-9);
        let dot = (side.x - tip.x) * (side.x - centre.x) + (side.y - tip.y) * (side.y - centre.y);
        assert!(dot.abs() < 1e-9, "the side is a tangent");
    }

    #[test]
    fn a_layered_rect_s_label_keeps_to_its_front_box() {
        let r = Rect::new(0.0, 0.0, 100.0, 60.0);
        let shape = "mxgraph.basic.layered_rect";
        let bounds = |s: &str| label_bounds(shape, r, &style(s, false), false);
        assert_eq!(bounds("dx=10;"), r, "only with boundedLbl");
        assert_eq!(
            bounds("dx=10;boundedLbl=1;"),
            Rect::new(0.0, 0.0, 90.0, 50.0)
        );
    }
}
