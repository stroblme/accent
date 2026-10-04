// Derived from draw.io js/grapheditor/Shapes.js and mxgraph/src/shape/mxArrowConnector.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! The shapes draw.io adds to mxGraph's (`Shapes.js`).

use super::mxgraph::{ellipse, rectangle};
use super::{
    Direction, Fill, LINE_ARCSIZE, Margins, Part, Pen, RECTANGLE_ROUNDING_FACTOR, add_points,
    polygon, polyline, quarter, rect,
};
use crate::geom::{PathCmd, Point, Rect};
use crate::scene::Cap;
use crate::style::Resolved;

/// `mxConstants.ARROW_SIZE`: a flex arrow's head is `ARROW_SIZE / 5 · 3` long by default.
const ARROW_SIZE: f64 = 30.0;

/// A `flexArrow` edge: a filled, stroked band along `points` with its heads.
///
/// `mxArrowConnector.paintEdgeShape` (mxArrowConnector.js 124-417) with `FlexArrowShape`'s
/// widths (Shapes.js 3990-4018): the band's outbound side is walked first, then its inbound side
/// back, each waypoint's corner on the mitre of the two segments.
// ponytail: `rounded=1` joins (quads at each bend) and `curved=1` (a fine polyline from
// `getCurvePoints`, trimmed for the heads) are drawn straight and mitred. The JS also lowers
// the miter limit to 1.42 on bent arrows and restrokes the heads at 4; `Part` has no miter
// limit, so the scene's own applies.
pub fn flex_arrow(points: &[Point], style: &Resolved, stroke_width: f64) -> Vec<Part> {
    let (Some(&p0), Some(&p1), Some(&pe)) = (points.first(), points.get(1), points.last()) else {
        return Vec::new();
    };
    let edge_width = style.num("width", 10.0) + (stroke_width - 1.0).max(0.0);
    let start_width = edge_width + style.num("startWidth", 20.0) + stroke_width;
    let end_width = edge_width + style.num("endWidth", 20.0) + stroke_width;
    // `none` is gone from a resolved style, so any value left is a head.
    let marker_start = style.get("startArrow").is_some();
    let marker_end = style.get("endArrow").is_some();
    // `arrowSpacing` is `mxConstants.ARROW_SPACING`, 0.
    let spacing = stroke_width / 2.0;
    let start_size = style.num("startSize", ARROW_SIZE / 5.0) * 3.0 + stroke_width;
    let end_size = style.num("endSize", ARROW_SIZE / 5.0) * 3.0 + stroke_width;

    let (dx, dy) = (p1.x - p0.x, p1.y - p0.y);
    let dist = dx.hypot(dy);
    if dist == 0.0 {
        return Vec::new();
    }
    let (mut nx, mut ny) = (dx / dist, dy / dist);
    let (mut nx1, mut ny1) = (nx, ny);
    let (orthx, orthy) = (edge_width * ny, -edge_width * nx);
    let mut path = Vec::new();
    // The inbound side's corners, drawn in reverse once the far end is reached.
    let mut inbound = Vec::new();

    if marker_start {
        let head = arrow_head(p0, (nx, ny), spacing, start_size, edge_width, start_width);
        path.push(PathCmd::MoveTo(head[0]));
        path.extend(head[1..].iter().map(|&p| PathCmd::LineTo(p)));
    } else {
        let out_start = Point::new(
            p0.x + orthx / 2.0 + spacing * nx,
            p0.y + orthy / 2.0 + spacing * ny,
        );
        let in_end = Point::new(
            p0.x - orthx / 2.0 + spacing * nx,
            p0.y - orthy / 2.0 + spacing * ny,
        );
        path.push(PathCmd::MoveTo(in_end));
        path.push(PathCmd::LineTo(out_start));
    }

    for w in points.windows(3) {
        let (dx1, dy1) = (w[2].x - w[1].x, w[2].y - w[1].y);
        let dist1 = dx1.hypot(dy1);
        if dist1 == 0.0 {
            continue;
        }
        (nx1, ny1) = (dx1 / dist1, dy1 / dist1);
        // The cosine of half the bend: how much further out than half the width the mitre lies.
        let angle_factor = ((nx * nx1 + ny * ny1 + 1.0) / 2.0).sqrt().max(0.06);
        let (nx2, ny2) = (nx + nx1, ny + ny1);
        let dist2 = nx2.hypot(ny2);
        if dist2 == 0.0 {
            continue;
        }
        let (nx2, ny2) = (nx2 / dist2, ny2 / dist2);
        let d = edge_width / 2.0 / angle_factor;
        path.push(PathCmd::LineTo(Point::new(
            w[1].x + ny2 * d,
            w[1].y - nx2 * d,
        )));
        inbound.push(Point::new(w[1].x - ny2 * d, w[1].y + nx2 * d));
        (nx, ny) = (nx1, ny1);
    }

    let (orthx, orthy) = (edge_width * ny1, -edge_width * nx1);
    if marker_end {
        let head = arrow_head(pe, (-nx, -ny), spacing, end_size, edge_width, end_width);
        path.extend(head.map(PathCmd::LineTo));
    } else {
        path.push(PathCmd::LineTo(Point::new(
            pe.x - spacing * nx1 + orthx / 2.0,
            pe.y - spacing * ny1 + orthy / 2.0,
        )));
        path.push(PathCmd::LineTo(Point::new(
            pe.x - spacing * nx1 - orthx / 2.0,
            pe.y - spacing * ny1 - orthy / 2.0,
        )));
    }
    path.extend(inbound.into_iter().rev().map(PathCmd::LineTo));
    path.push(PathCmd::Close);
    vec![Part::body(path)]
}

/// A flex arrow's head at `pt`, `n` pointing from `pt` into the band
/// (`mxArrowConnector.paintMarker`, mxArrowConnector.js 424-443): out from the band's side to the
/// head's width, to the tip, and back in to the other side.
fn arrow_head(
    pt: Point,
    (nx, ny): (f64, f64),
    spacing: f64,
    size: f64,
    edge_width: f64,
    arrow_width: f64,
) -> [Point; 5] {
    let (orthx, orthy) = (edge_width * ny / 2.0, -edge_width * nx / 2.0);
    // `orth / widthArrowRatio` in the JS, without dividing by a zero-width band.
    let (wingx, wingy) = (arrow_width * ny / 2.0, -arrow_width * nx / 2.0);
    let (spacex, spacey) = ((spacing + size) * nx, (spacing + size) * ny);
    [
        Point::new(pt.x - orthx + spacex, pt.y - orthy + spacey),
        Point::new(pt.x - wingx + spacex, pt.y - wingy + spacey),
        Point::new(pt.x + spacing * nx, pt.y + spacing * ny),
        Point::new(pt.x + wingx + spacex, pt.y + wingy + spacey),
        Point::new(pt.x + orthx + spacex, pt.y + orthy + spacey),
    ]
}

/// `note`: a sheet with its top-right corner folded down by `size` (`NoteShape`, Shapes.js
/// 758-812): the outline, the fold shaded by `darkOpacity`, then the fold stroked over it.
pub(super) fn note(b: Rect, style: &Resolved) -> Vec<Part> {
    let s = style.num("size", 30.0).min(b.h).min(b.w).max(0.0);
    let (w, h) = (b.w, b.h);
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    let mut parts = vec![Part::body(vec![
        PathCmd::MoveTo(p(0.0, 0.0)),
        PathCmd::LineTo(p(w - s, 0.0)),
        PathCmd::LineTo(p(w, s)),
        PathCmd::LineTo(p(w, h)),
        PathCmd::LineTo(p(0.0, h)),
        PathCmd::LineTo(p(0.0, 0.0)),
        PathCmd::Close,
    ])];
    let fold = [p(w - s, 0.0), p(w - s, s), p(w, s)];
    parts.extend(shade(&fold, style, "darkOpacity"));
    parts.push(Part::line(polyline(&fold)));
    parts
}

/// The closed outline `pts` shaded by the style's `key` (`darkOpacity`), none when it is 0.
fn shade(pts: &[Point], style: &Resolved, key: &str) -> Option<Part> {
    let op = style.num(key, 0.0).clamp(-1.0, 1.0);
    let mut path = polyline(pts);
    path.push(PathCmd::Close);
    (op != 0.0).then(|| Part::filled(path, Fill::Shade(op)))
}

/// `cylinder3`: a can whose top and bottom are half ellipses `size` high (`CylinderShape3`,
/// Shapes.js 921-982). With `lid` (the default) the body's top is the rim's back half and its
/// front half is stroked over the body; without, the top is open and only the front half shows.
/// Each `arcTo` is a quarter ellipse, drawn with [`quarter`].
pub(super) fn cylinder(b: Rect, style: &Resolved) -> Vec<Part> {
    let size = style.num("size", 15.0).min(b.h * 0.5).max(0.0);
    if size == 0.0 {
        return vec![Part::body(rect(b))];
    }
    let (w, h) = (b.w, b.h);
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    let lid = style.flag("lid", true);
    let mut body = if lid {
        vec![
            PathCmd::MoveTo(p(0.0, size)),
            quarter(p(0.0, size), p(0.0, 0.0), p(w / 2.0, 0.0)),
            quarter(p(w / 2.0, 0.0), p(w, 0.0), p(w, size)),
        ]
    } else {
        vec![
            PathCmd::MoveTo(p(0.0, 0.0)),
            quarter(p(0.0, 0.0), p(0.0, size), p(w / 2.0, size)),
            quarter(p(w / 2.0, size), p(w, size), p(w, 0.0)),
        ]
    };
    body.extend([
        PathCmd::LineTo(p(w, h - size)),
        quarter(p(w, h - size), p(w, h), p(w / 2.0, h)),
        quarter(p(w / 2.0, h), p(0.0, h), p(0.0, h - size)),
        PathCmd::Close,
    ]);
    let mut parts = vec![Part::body(body)];
    if lid {
        parts.push(Part::line(vec![
            PathCmd::MoveTo(p(w, size)),
            quarter(p(w, size), p(w, 2.0 * size), p(w / 2.0, 2.0 * size)),
            quarter(p(w / 2.0, 2.0 * size), p(0.0, 2.0 * size), p(0.0, size)),
        ]));
    }
    parts
}

/// `curlyBracket`: an open `{` with its tip on the left edge and its spine `size` of the width
/// in (`CurlyBracketShape`, Shapes.js 1638-1658, painted by `mxActor.paintVertexShape`,
/// mxActor.js 64-70). It has no fill.
pub(super) fn curly_bracket(b: Rect, style: &Resolved) -> Vec<Part> {
    let s = b.w * style.num("size", 0.5).clamp(0.0, 1.0);
    let (w, h) = (b.w, b.h);
    let arc = style.num("arcSize", LINE_ARCSIZE) / 2.0;
    let pts = [
        (w, 0.0),
        (s, 0.0),
        (s, h / 2.0),
        (0.0, h / 2.0),
        (s, h / 2.0),
        (s, h),
        (w, h),
    ]
    .map(|(x, y)| Point::new(b.x + x, b.y + y));
    vec![Part::line(add_points(
        &pts,
        style.flag("rounded", false),
        arc,
        false,
    ))]
}

/// `size` as the fixed-size shapes read it: with `fixedSize=1` in page units (`fixed` by
/// default) and at most `max`, otherwise a share of `extent` (`share` by default) no more than
/// `limit` of it.
fn inset(
    style: &Resolved,
    extent: f64,
    (fixed, max): (f64, f64),
    (share, limit): (f64, f64),
) -> f64 {
    match style.flag("fixedSize", false) {
        true => style.num("size", fixed).min(max).max(0.0),
        false => extent * style.num("size", share).min(limit).max(0.0),
    }
}

/// `process`: a rectangle with a bar `size` of the width in at either side (`ProcessShape`,
/// Shapes.js 1969-2057).
pub(super) fn process(b: Rect, style: &Resolved) -> Vec<Part> {
    // Whole units, as draw.io keeps the inner lines crisp.
    let i = process_inset(b, style, false).round();
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    let mut bars = polyline(&[p(i, 0.0), p(i, b.h)]);
    bars.extend(polyline(&[p(b.w - i, 0.0), p(b.w - i, b.h)]));
    vec![Part::body(rectangle(b, style)), Part::line(bars)]
}

/// How far a process's bars are in: `size` of the width (page units with `fixedSize`), and at
/// least a rounded corner.
fn process_inset(b: Rect, style: &Resolved, label: bool) -> f64 {
    let (w, h) = (b.w, b.h);
    let size = style.num("size", 0.1);
    let fixed = style.flag("fixedSize", false);
    let mut inset = match fixed {
        true => size.min(w).max(0.0),
        false => w * size.clamp(0.0, 1.0),
    };
    // The label keeps a fixed bar's width alone (Shapes.js 1998-2012).
    if style.flag("rounded", false) && !(label && fixed) {
        let f = style.num("arcSize", RECTANGLE_ROUNDING_FACTOR * 100.0) / 100.0;
        inset = inset.max((w * f).min(h * f));
    }
    inset
}

/// A process's label keeps between its bars, when it runs along them.
pub(super) fn process_label(rect: Rect, style: &Resolved) -> Rect {
    let along = !Direction::of(style).vertical();
    if style.flag("horizontal", true) != along {
        return rect;
    }
    let i = process_inset(rect, style, true);
    Rect::new(
        rect.x + i.round(),
        rect.y,
        rect.w - (2.0 * i).round(),
        rect.h,
    )
}

/// `parallelogram`: leaning right by `size` of the width (`ParallelogramShape`, Shapes.js
/// 1578-1605).
pub(super) fn parallelogram(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let dx = inset(style, w, (20.0, w), (0.2, 1.0));
    let pts = [(0.0, h), (dx, 0.0), (w, 0.0), (w - dx, h)];
    vec![Part::body(polygon(b, style, &pts, &[]))]
}

/// `trapezoid`: its top `size` of the width shorter at each end (`TrapezoidShape`, Shapes.js
/// 1608-1635).
pub(super) fn trapezoid(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let dx = inset(style, w, (20.0, w * 0.5), (0.2, 0.5));
    let pts = [(0.0, h), (dx, 0.0), (w - dx, 0.0), (w, h)];
    vec![Part::body(polygon(b, style, &pts, &[]))]
}

/// `step`: a chevron, `size` of the width deep (`StepShape`, Shapes.js 2283-2310).
pub(super) fn step(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let s = inset(style, w, (20.0, w), (0.2, 1.0));
    let pts = [
        (0.0, 0.0),
        (w - s, 0.0),
        (w, h / 2.0),
        (w - s, h),
        (0.0, h),
        (s, h / 2.0),
    ];
    vec![Part::body(polygon(b, style, &pts, &[]))]
}

/// `hexagon`: pointed left and right, its corners `size` of the width in (draw.io's
/// `HexagonShape`, Shapes.js 2313-2338, in place of mxGraph's `mxHexagon`).
pub(super) fn hexagon(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let s = inset(style, w, (20.0, w * 0.5), (0.25, 1.0));
    let pts = [
        (s, 0.0),
        (w - s, 0.0),
        (w, h / 2.0),
        (w - s, h),
        (s, h),
        (0.0, h / 2.0),
    ];
    vec![Part::body(polygon(b, style, &pts, &[]))]
}

/// `document`: a sheet whose foot is a wave `size` of the height deep (`DocumentShape`,
/// Shapes.js 1424-1459).
pub(super) fn document(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let dy = h * style.num("size", 0.3).clamp(0.0, 1.0);
    let fy = 1.4;
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    vec![Part::body(vec![
        PathCmd::MoveTo(p(0.0, 0.0)),
        PathCmd::LineTo(p(w, 0.0)),
        PathCmd::LineTo(p(w, h - dy / 2.0)),
        PathCmd::QuadTo(p(w * 3.0 / 4.0, h - dy * fy), p(w / 2.0, h - dy / 2.0)),
        PathCmd::QuadTo(p(w / 4.0, h - dy * (1.0 - fy)), p(0.0, h - dy / 2.0)),
        PathCmd::LineTo(p(0.0, dy / 2.0)),
        PathCmd::Close,
    ])]
}

/// With `boundedLbl=1` a document's label keeps above its wave.
pub(super) fn document_margins(rect: Rect, style: &Resolved) -> Margins {
    Margins {
        bottom: style.num("size", 0.3) * rect.h,
        ..Margins::default()
    }
}

/// `internalStorage`: a rectangle with a line `dy` below its top and one `dx` in from its left
/// (`InternalStorageShape`, Shapes.js 4059-4103).
pub(super) fn internal_storage(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let round = match style.flag("rounded", false) {
        true => {
            let f = style.num("arcSize", RECTANGLE_ROUNDING_FACTOR * 100.0) / 100.0;
            (w * f).min(h * f).max(0.0)
        }
        false => 0.0,
    };
    let dx = style.num("dx", 20.0).min(w).max(round);
    let dy = style.num("dy", 20.0).min(h).max(round);
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    vec![
        Part::body(rectangle(b, style)),
        Part::line(polyline(&[p(0.0, dy), p(w, dy)])),
        Part::line(polyline(&[p(dx, 0.0), p(dx, h)])),
    ]
}

/// `cube`: a box seen from above and the left, its top and left sides `size` deep and shaded by
/// `darkOpacity` and `darkOpacity2` (`CubeShape`, Shapes.js 460-545).
pub(super) fn cube(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let s = style.num("size", 20.0).min(w.min(h)).max(0.0);
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    let body = [
        p(0.0, 0.0),
        p(w - s, 0.0),
        p(w, s),
        p(w, h),
        p(s, h),
        p(0.0, h - s),
        p(0.0, 0.0),
    ];
    let mut outline = polyline(&body);
    outline.push(PathCmd::Close);
    let mut parts = vec![Part::body(outline)];
    let top = [p(0.0, 0.0), p(w - s, 0.0), p(w, s), p(s, s)];
    let side = [p(0.0, 0.0), p(s, s), p(s, h), p(0.0, h - s)];
    parts.extend(shade(&top, style, "darkOpacity"));
    parts.extend(shade(&side, style, "darkOpacity2"));
    let mut edges = polyline(&[p(s, h), p(s, s), p(0.0, 0.0)]);
    edges.extend(polyline(&[p(s, s), p(w, s)]));
    parts.push(Part::line(edges));
    parts
}

/// With `boundedLbl=1` a cube's label keeps off its top and left sides.
pub(super) fn cube_margins(style: &Resolved) -> Margins {
    let s = style.num("size", 20.0);
    Margins {
        left: s,
        top: s,
        ..Margins::default()
    }
}

/// `tape`: a band whose top and bottom are waves `size` of the height deep (`TapeShape`,
/// Shapes.js 1369-1421).
pub(super) fn tape(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let dy = h * style.num("size", 0.4).clamp(0.0, 1.0);
    let fy = 1.4;
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    vec![Part::body(vec![
        PathCmd::MoveTo(p(0.0, dy / 2.0)),
        PathCmd::QuadTo(p(w / 4.0, dy * fy), p(w / 2.0, dy / 2.0)),
        PathCmd::QuadTo(p(w * 3.0 / 4.0, dy * (1.0 - fy)), p(w, dy / 2.0)),
        PathCmd::LineTo(p(w, h - dy / 2.0)),
        PathCmd::QuadTo(p(w * 3.0 / 4.0, h - dy * fy), p(w / 2.0, h - dy / 2.0)),
        PathCmd::QuadTo(p(w / 4.0, h - dy * (1.0 - fy)), p(0.0, h - dy / 2.0)),
        PathCmd::LineTo(p(0.0, dy / 2.0)),
        PathCmd::Close,
    ])]
}

/// With `boundedLbl=1` a tape's label keeps between its waves (Shapes.js 1394-1419).
pub(super) fn tape_label(rect: Rect, style: &Resolved) -> Rect {
    if !style.flag("boundedLbl", false) {
        return rect;
    }
    let size = style.num("size", 0.4);
    match Direction::of(style).vertical() {
        false => {
            let dy = rect.h * size;
            Rect::new(rect.x, rect.y + dy, rect.w, rect.h - 2.0 * dy)
        }
        true => {
            let dx = rect.w * size;
            Rect::new(rect.x + dx, rect.y, rect.w - 2.0 * dx, rect.h)
        }
    }
}

/// `card`: its top-left corner cut off `size` deep (`CardShape`, Shapes.js 1343-1366).
pub(super) fn card(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let s = style.num("size", 30.0).min(w.min(h)).max(0.0);
    let pts = [(s, 0.0), (w, 0.0), (w, h), (0.0, h), (0.0, s)];
    vec![Part::body(polygon(b, style, &pts, &[]))]
}

/// `callout`: a box over a tail `size` high whose base starts `position` of the width along,
/// `base` wide, and whose tip is `position2` along (`CalloutShape`, Shapes.js 2081-2121); the tip
/// stays sharp when the corners are rounded.
pub(super) fn callout(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let s = style.num("size", 30.0).min(h).max(0.0);
    let dx = w * style.num("position", 0.5).clamp(0.0, 1.0);
    let dx2 = w * style.num("position2", 0.5).clamp(0.0, 1.0);
    let base = style.num("base", 20.0).min(w).max(0.0);
    let pts = [
        (0.0, 0.0),
        (w, 0.0),
        (w, h - s),
        (w.min(dx + base), h - s),
        (dx2, h),
        (dx.max(0.0), h - s),
        (0.0, h - s),
    ];
    vec![Part::body(polygon(b, style, &pts, &[4]))]
}

/// A callout's label keeps above its tail, `boundedLbl` or not.
pub(super) fn callout_margins(style: &Resolved) -> Margins {
    Margins {
        bottom: style.num("size", 30.0),
        ..Margins::default()
    }
}

/// `wedgeCallout`: a box with a tail to a tip at `tipX`/`tipY` (in widths and heights from the
/// centre), out of the side the tip lies beyond, `base` wide; no tail while the tip is inside
/// (`WedgeCalloutShape`, Shapes.js 2124-2280).
pub(super) fn wedge_callout(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let rounded = style.flag("rounded", false);
    // Kept within a hundred times the size, so the box around it stays finite.
    let tx = style.num("tipX", -0.25).clamp(-100.0, 100.0);
    let ty = style.num("tipY", 1.0).clamp(-100.0, 100.0);
    let (dx, dy) = (tx * w, ty * h);
    let mut pts = vec![(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)];
    let mut exclude = Vec::new();
    if dx.abs() > w / 2.0 || dy.abs() > h / 2.0 {
        let base = style.num("base", 20.0).max(0.0);
        let arc = if rounded {
            style.num("arcSize", LINE_ARCSIZE) / 2.0
        } else {
            0.0
        };
        let tip = (w / 2.0 + dx, h / 2.0 + dy);
        let (tail, at) = if dx.abs() * h >= dy.abs() * w && dx != 0.0 {
            // Out of the left or right side.
            let inset = arc.min(h / 2.0);
            let hb = base.min(h - 2.0 * inset).max(0.0) / 2.0;
            let ey = (h / 2.0 + dy * (w / 2.0) / dx.abs())
                .min(h - inset - hb)
                .max(inset + hb);
            match dx > 0.0 {
                true => ([(w, ey - hb), tip, (w, ey + hb)], 2),
                false => ([(0.0, ey + hb), tip, (0.0, ey - hb)], 4),
            }
        } else {
            // Out of the top or bottom.
            let inset = arc.min(w / 2.0);
            let hb = base.min(w - 2.0 * inset).max(0.0) / 2.0;
            let ex = (w / 2.0 + dx * (h / 2.0) / dy.abs())
                .min(w - inset - hb)
                .max(inset + hb);
            match dy > 0.0 {
                true => ([(ex + hb, h), tip, (ex - hb, h)], 3),
                false => ([(ex - hb, 0.0), tip, (ex + hb, 0.0)], 1),
            }
        };
        pts.splice(at..at, tail);
        exclude = vec![at, at + 1, at + 2];
    }
    vec![Part::body(polygon(b, style, &pts, &exclude))]
}

/// `umlActor`: a stick figure, its head filled (`UmlActorShape`, Shapes.js 2642-2678).
pub(super) fn uml_actor(b: Rect) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    let head = Rect::new(b.x + w / 4.0, b.y, w / 2.0, h / 4.0);
    let (neck, hips, arms) = (
        p(w / 2.0, h / 4.0),
        p(w / 2.0, 2.0 * h / 3.0),
        p(w / 2.0, h / 3.0),
    );
    let mut limbs = polyline(&[neck, hips]);
    limbs.extend(polyline(&[arms, p(0.0, h / 3.0)]));
    limbs.extend(polyline(&[arms, p(w, h / 3.0)]));
    limbs.extend(polyline(&[hips, p(0.0, h)]));
    limbs.extend(polyline(&[hips, p(w, h)]));
    vec![Part::body(ellipse(head)), Part::line(limbs)]
}

/// `or`: a logic gate's OR outline, flat at the left (`OrShape`, Shapes.js 4289-4305).
pub(super) fn or(b: Rect) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    vec![Part::body(vec![
        PathCmd::MoveTo(p(0.0, 0.0)),
        PathCmd::QuadTo(p(w, 0.0), p(w, h / 2.0)),
        PathCmd::QuadTo(p(w, h), p(0.0, h)),
        PathCmd::Close,
    ])]
}

/// `xor`: [`or`] with its left side bowed in (`XorShape`, Shapes.js 4308-4325).
pub(super) fn xor(b: Rect) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    vec![Part::body(vec![
        PathCmd::MoveTo(p(0.0, 0.0)),
        PathCmd::QuadTo(p(w, 0.0), p(w, h / 2.0)),
        PathCmd::QuadTo(p(w, h), p(0.0, h)),
        PathCmd::QuadTo(p(w / 2.0, h / 2.0), p(0.0, 0.0)),
        PathCmd::Close,
    ])]
}

/// `dataStorage`: a drum lying on its side, its ends bowed left `size` of the width
/// (`DataStorageShape`, Shapes.js 4260-4286).
pub(super) fn data_storage(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let s = inset(style, w, (20.0, w), (0.1, 1.0));
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    vec![Part::body(vec![
        PathCmd::MoveTo(p(s, 0.0)),
        PathCmd::LineTo(p(w, 0.0)),
        PathCmd::QuadTo(p(w - s * 2.0, h / 2.0), p(w, h)),
        PathCmd::LineTo(p(s, h)),
        PathCmd::QuadTo(p(s - s * 2.0, h / 2.0), p(s, 0.0)),
        PathCmd::Close,
    ])]
}

/// `message`: an envelope, its flap stroked over the box (`MessageShape`, Shapes.js 2613-2639,
/// painted by `mxCylinder.paintVertexShape`).
pub(super) fn message(b: Rect) -> Vec<Part> {
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    let flap = [p(0.0, 0.0), p(b.w / 2.0, b.h / 2.0), p(b.w, 0.0)];
    vec![Part::body(rect(b)), Part::line(polyline(&flap))]
}

/// With `boundedLbl=1` a `cylinder3`'s label keeps below its lid and above its foot
/// (Shapes.js 1489-1504).
pub(super) fn cylinder_margins(rect: Rect, style: &Resolved) -> Margins {
    let mut size = style.num("size", 15.0);
    if !style.flag("lid", true) {
        size /= 2.0;
    }
    Margins {
        top: rect.h.min(size * 2.0),
        bottom: (size * 0.3).max(0.0),
        ..Margins::default()
    }
}

/// `partialRectangle`: a filled box stroked only along the sides `top`, `right`, `bottom` and
/// `left` leave on, all by default, capped square so that two sides meet on a full corner
/// (`PartialRectangleShape`, Shapes.js 5248-5340), as ER tables draw their rows.
// ponytail: a table cell's grid lines drawn back over its fill (`paintTableCellLines`) are not
// drawn; tables are not ported.
pub(super) fn partial_rectangle(b: Rect, style: &Resolved) -> Vec<Part> {
    let side = |key: &str| style.get(key).is_none_or(|v| v == "1");
    let corners = [
        Point::new(b.x, b.y),
        Point::new(b.right(), b.y),
        Point::new(b.right(), b.bottom()),
        Point::new(b.x, b.bottom()),
    ];
    // Each side on draws a line from the corner before it; one off moves on.
    let mut sides = vec![PathCmd::MoveTo(corners[0])];
    for (i, key) in ["top", "right", "bottom", "left"].into_iter().enumerate() {
        let to = corners[(i + 1) % 4];
        sides.push(match side(key) {
            true => PathCmd::LineTo(to),
            false => PathCmd::MoveTo(to),
        });
    }
    let square = Pen {
        cap: Cap::Square,
        ..Pen::default()
    };
    vec![
        Part::filled(rect(b), Fill::Cell),
        Part::line(sides).with(square),
    ]
}

/// `folder`: a box with a tab `tabWidth` by `tabHeight` on its top, at the right unless
/// `tabPosition=left`, and a small triangle under the tab with `folderSymbol=triangle`
/// (`FolderShape`, Shapes.js 1006-1107). Rounded, its corners are `arcSize` of the shorter side
/// (units with `absoluteArcSize`).
pub(super) fn folder(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let tab_h = style.num("tabHeight", 20.0).min(h).max(0.0);
    let arc = folder_arc(w, h, tab_h, style);
    let tab_w = style
        .num("tabWidth", 60.0)
        .min(w)
        .max(0.0)
        .max(arc)
        .min(w - arc);
    let r = if style.flag("rounded", false) {
        arc
    } else {
        0.0
    };
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    let mut path = match style.get("tabPosition") {
        Some("left") => polyline(&[p(r, tab_h), p(r, 0.0), p(tab_w, 0.0), p(tab_w, tab_h)]),
        _ => polyline(&[
            p(w - tab_w, tab_h),
            p(w - tab_w, 0.0),
            p(w - r, 0.0),
            p(w - r, tab_h),
        ]),
    };
    if r > 0.0 {
        path.extend([
            PathCmd::MoveTo(p(0.0, r + tab_h)),
            quarter(p(0.0, r + tab_h), p(0.0, tab_h), p(r, tab_h)),
            PathCmd::LineTo(p(w - r, tab_h)),
            quarter(p(w - r, tab_h), p(w, tab_h), p(w, r + tab_h)),
            PathCmd::LineTo(p(w, h - r)),
            quarter(p(w, h - r), p(w, h), p(w - r, h)),
            PathCmd::LineTo(p(r, h)),
            quarter(p(r, h), p(0.0, h), p(0.0, h - r)),
        ]);
    } else {
        path.extend(polyline(&[p(0.0, tab_h), p(w, tab_h), p(w, h), p(0.0, h)]));
    }
    path.push(PathCmd::Close);
    let mut parts = vec![Part::body(path)];
    if style.get("folderSymbol") == Some("triangle") {
        let mut mark = polyline(&[
            p(w - 30.0, tab_h + 20.0),
            p(w - 20.0, tab_h + 10.0),
            p(w - 10.0, tab_h + 20.0),
        ]);
        mark.push(PathCmd::Close);
        parts.push(Part::line(mark));
    }
    parts
}

/// A folder's corner: `arcSize` (a share, 0.1 by default) of the shorter side, or units with
/// `absoluteArcSize`, within half the width and half the body.
fn folder_arc(w: f64, h: f64, tab_h: f64, style: &Resolved) -> f64 {
    let mut arc = style.num("arcSize", 0.1);
    if !style.flag("absoluteArcSize", false) {
        arc *= w.min(h);
    }
    arc.min(w * 0.5).min((h - tab_h) * 0.5)
}

/// With `boundedLbl=1` a folder's label keeps below the tab, or with `labelInHeader=1` in it
/// (Shapes.js 1506-1548).
pub(super) fn folder_margins(rect: Rect, style: &Resolved) -> Margins {
    let tab_h = style.num("tabHeight", 15.0);
    if !style.flag("labelInHeader", false) {
        return Margins {
            top: rect.h.min(tab_h),
            ..Margins::default()
        };
    }
    let tab_w = style.num("tabWidth", 15.0);
    let arc = match style.flag("rounded", false) {
        true => folder_arc(rect.w, rect.h, tab_h, style),
        false => 0.0,
    };
    let (beside, below) = (rect.w.min(rect.w - tab_w), rect.h.min(rect.h - tab_h));
    match style.get("tabPosition") {
        Some("left") => Margins {
            left: arc,
            top: 0.0,
            right: beside,
            bottom: below,
        },
        _ => Margins {
            left: beside,
            top: 0.0,
            right: arc,
            bottom: below,
        },
    }
}

/// `component`: UML's component box with two jetties `jettyWidth` by `jettyHeight` on its left
/// (`ComponentShape`, Shapes.js 3841-3892, painted as `mxCylinder`).
pub(super) fn component(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let dx = style.num("jettyWidth", 32.0);
    let dy = style.num("jettyHeight", 12.0);
    let (x0, x1) = (dx / 2.0, dx);
    let (y0, y1) = (0.3 * h - dy / 2.0, 0.7 * h - dy / 2.0);
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    let mut body = polyline(&[
        p(x0, 0.0),
        p(w, 0.0),
        p(w, h),
        p(x0, h),
        p(x0, y1 + dy),
        p(0.0, y1 + dy),
        p(0.0, y1),
        p(x0, y1),
        p(x0, y0 + dy),
        p(0.0, y0 + dy),
        p(0.0, y0),
        p(x0, y0),
    ]);
    body.push(PathCmd::Close);
    let mut jetties = polyline(&[p(x0, y0), p(x1, y0), p(x1, y0 + dy), p(x0, y0 + dy)]);
    jetties.extend(polyline(&[
        p(x0, y1),
        p(x1, y1),
        p(x1, y1 + dy),
        p(x0, y1 + dy),
    ]));
    vec![Part::body(body), Part::line(jetties)]
}

/// `plus`: a box with a cross in it (`PlusShape`, Shapes.js 2341-2367).
pub(super) fn plus(b: Rect, style: &Resolved) -> Vec<Part> {
    let border = (b.w / 5.0).min(b.h / 5.0) + 1.0;
    let c = b.centre();
    let mut cross = polyline(&[
        Point::new(c.x, b.y + border),
        Point::new(c.x, b.bottom() - border),
    ]);
    cross.extend(polyline(&[
        Point::new(b.x + border, c.y),
        Point::new(b.right() - border, c.y),
    ]));
    vec![Part::body(rectangle(b, style)), Part::line(cross)]
}

/// `startState` and `endState`: UML's initial state, a filled disc a little inside its box, and
/// its final state, the same in a ring (`StateShape` and `StartStateShape`, Shapes.js
/// 3919-3958).
pub(super) fn state(b: Rect, ring: bool) -> Vec<Part> {
    let inset = 4.0_f64.min(b.w / 5.0).min(b.h / 5.0);
    let mut parts = Vec::new();
    if b.w > 0.0 && b.h > 0.0 {
        let disc = Rect::new(
            b.x + inset,
            b.y + inset,
            b.w - 2.0 * inset,
            b.h - 2.0 * inset,
        );
        parts.push(Part::body(ellipse(disc)));
    }
    if ring {
        parts.push(Part::line(ellipse(b)));
    }
    parts
}

/// `offPageConnector`: a box whose foot is a point `size` of the height deep
/// (`OffPageConnectorShape`, Shapes.js 4354-4373).
pub(super) fn off_page_connector(b: Rect, style: &Resolved) -> Vec<Part> {
    let (w, h) = (b.w, b.h);
    let s = h * style.num("size", 3.0 / 8.0).clamp(0.0, 1.0);
    let pts = [(0.0, 0.0), (w, 0.0), (w, h - s), (w / 2.0, h), (0.0, h - s)];
    vec![Part::body(polygon(b, style, &pts, &[]))]
}

/// `waypoint`: a dot `size` across in the stroke's colour, grown by the stroke, in a box that
/// takes clicks and is not drawn (`WaypointShape`, Shapes.js 603-624).
pub(super) fn waypoint(b: Rect, style: &Resolved) -> Vec<Part> {
    let s = (style.num("size", 6.0) - 2.0).max(0.0) + 2.0 * style.num("strokeWidth", 1.0);
    let dot = Rect::new(b.x + (b.w - s) * 0.5, b.y + (b.h - s) * 0.5, s, s);
    let ink = style.color("strokeColor").map_or(Fill::None, Fill::Own);
    vec![
        Part::filled(ellipse(dot), ink),
        Part::filled(rect(b), Fill::None),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::path_bounds;
    use crate::shapes::tests::{BOX, near, same_box, style};
    use crate::shapes::vertex;

    #[test]
    fn note_has_a_fold() {
        let parts = vertex(
            "note",
            Rect::new(0.0, 0.0, 100.0, 60.0),
            &style("shape=note;", false),
        );
        assert_eq!(parts.len(), 2);
        assert!(parts[0].fill == Fill::Cell && parts[0].stroke);
        assert!(parts[1].fill == Fill::None && parts[1].stroke);
        assert_eq!(
            parts[1].path,
            vec![
                PathCmd::MoveTo(Point::new(70.0, 0.0)),
                PathCmd::LineTo(Point::new(70.0, 30.0)),
                PathCmd::LineTo(Point::new(100.0, 30.0)),
            ]
        );
    }

    #[test]
    fn cylinder_body_spans_the_bounds() {
        let parts = vertex("cylinder3", BOX, &style("shape=cylinder3;size=10;", false));
        assert_eq!(parts.len(), 2);
        assert!(same_box(path_bounds(&parts[0].path).unwrap(), BOX));
        assert!(parts[1].fill == Fill::None && parts[1].stroke);
        let lid = path_bounds(&parts[1].path).unwrap();
        assert!(
            (lid.bottom() - (BOX.y + 20.0)).abs() < 1e-9,
            "the lid's front reaches 2·size down"
        );
        let open = vertex("cylinder3", BOX, &style("shape=cylinder3;lid=0;", false));
        assert_eq!(open.len(), 1);
    }

    #[test]
    fn curly_bracket_is_unfilled() {
        let parts = vertex("curlyBracket", BOX, &style("shape=curlyBracket;", false));
        assert_eq!(parts.len(), 1);
        assert!(parts[0].fill == Fill::None && parts[0].stroke);
        assert!(
            same_box(path_bounds(&parts[0].path).unwrap(), BOX),
            "unrounded, the tip touches the left side"
        );
    }

    #[test]
    fn flex_arrow_is_a_closed_band() {
        let pts = [Point::new(0.0, 0.0), Point::new(100.0, 0.0)];
        let parts = flex_arrow(&pts, &style("shape=flexArrow;", true), 1.0);
        assert_eq!(parts.len(), 1);
        assert!(parts[0].fill == Fill::Cell && parts[0].stroke);
        let path = &parts[0].path;
        assert!(matches!(path[0], PathCmd::MoveTo(_)));
        assert_eq!(path.last(), Some(&PathCmd::Close));
        // Band 10 wide from half a stroke in; the default end head (endArrow=classic) is
        // 10 + 20 + 1 wide with its tip half a stroke short of the end.
        let b = path_bounds(path).unwrap();
        assert!(same_box(b, Rect::new(0.5, -15.5, 99.0, 31.0)), "{b:?}");
        let bent = [
            Point::new(0.0, 0.0),
            Point::new(100.0, 0.0),
            Point::new(100.0, 100.0),
        ];
        let path = &flex_arrow(&bent, &style("shape=flexArrow;endArrow=none;", true), 1.0)[0].path;
        let on_path = |q: Point| {
            path.iter()
                .any(|c| matches!(c, PathCmd::LineTo(p) if near(*p, q)))
        };
        assert!(
            on_path(Point::new(95.0, 5.0)) && on_path(Point::new(105.0, -5.0)),
            "both sides meet on the mitre: {path:?}"
        );
    }

    fn corners(path: &[PathCmd]) -> Vec<Point> {
        path.iter()
            .filter_map(|c| match c {
                PathCmd::MoveTo(p) | PathCmd::LineTo(p) => Some(*p),
                _ => None,
            })
            .collect()
    }

    const R: Rect = Rect::new(0.0, 0.0, 100.0, 50.0);

    #[test]
    fn size_is_a_share_of_the_width_or_with_fixed_size_in_units() {
        let first = |s: &str| corners(&vertex("parallelogram", R, &style(s, false))[0].path)[1];
        assert_eq!(first("shape=parallelogram;"), Point::new(20.0, 0.0));
        assert_eq!(
            first("shape=parallelogram;size=0.3;"),
            Point::new(30.0, 0.0)
        );
        let fixed = "shape=parallelogram;fixedSize=1;size=12;";
        assert_eq!(first(fixed), Point::new(12.0, 0.0));
        let trapezoid =
            corners(&vertex("trapezoid", R, &style("shape=trapezoid;size=0.9;", false))[0].path);
        assert_eq!(
            trapezoid[2],
            Point::new(50.0, 0.0),
            "no more than half from each end"
        );
        let hexagon = corners(&vertex("hexagon", R, &style("shape=hexagon;", false))[0].path);
        assert_eq!(hexagon[0], Point::new(25.0, 0.0));
        let step = corners(&vertex("step", R, &style("shape=step;", false))[0].path);
        assert_eq!(step[5], Point::new(20.0, 25.0));
    }

    #[test]
    fn a_process_has_bars_its_label_keeps_between() {
        let parts = vertex("process", R, &style("shape=process;", false));
        let p = Point::new;
        assert_eq!(
            corners(&parts[1].path),
            [p(10.0, 0.0), p(10.0, 50.0), p(90.0, 0.0), p(90.0, 50.0)]
        );
        let s = style("shape=process;", false);
        assert_eq!(process_label(R, &s), Rect::new(10.0, 0.0, 80.0, 50.0));
        let across = style("shape=process;horizontal=0;", false);
        assert_eq!(process_label(R, &across), R);
    }

    #[test]
    fn a_callouts_tip_stays_sharp_and_its_label_above_the_tail() {
        let rounded = &vertex("callout", R, &style("shape=callout;rounded=1;", false))[0].path;
        assert!(
            rounded.contains(&PathCmd::LineTo(Point::new(50.0, 50.0))),
            "the tip is a corner of its own: {rounded:?}"
        );
        assert_eq!(
            rounded
                .iter()
                .filter(|c| matches!(c, PathCmd::QuadTo(..)))
                .count(),
            6
        );
        let s = style("shape=callout;", false);
        assert_eq!(
            super::super::label_bounds("callout", R, &s, false),
            Rect::new(0.0, 0.0, 100.0, 20.0)
        );
    }

    #[test]
    fn a_wedge_callouts_tail_leaves_the_side_its_tip_is_beyond() {
        let tail = |s: &str| {
            let path = &vertex("wedgeCallout", R, &style(s, false))[0].path;
            path_bounds(path).unwrap()
        };
        // The default tip is a quarter width left of the centre and a height below it.
        assert_eq!(
            tail("shape=wedgeCallout;"),
            Rect::new(0.0, 0.0, 100.0, 75.0)
        );
        assert_eq!(
            tail("shape=wedgeCallout;tipX=1;tipY=0;"),
            Rect::new(0.0, 0.0, 150.0, 50.0)
        );
        assert_eq!(
            tail("shape=wedgeCallout;tipX=0.2;tipY=0.2;"),
            R,
            "inside: no tail"
        );
    }

    #[test]
    fn a_cube_and_a_note_shade_their_sides_with_dark_opacity() {
        let s = "shape=cube;darkOpacity=0.05;darkOpacity2=-0.1;";
        let parts = vertex("cube", R, &style(s, false));
        let shades: Vec<Fill> = parts.iter().map(|p| p.fill).collect();
        assert_eq!(
            shades,
            [Fill::Cell, Fill::Shade(0.05), Fill::Shade(-0.1), Fill::None]
        );
        let plain = vertex("cube", R, &style("shape=cube;", false));
        assert_eq!(plain.len(), 2, "no shading at 0");
        let note = vertex("note", R, &style("shape=note;darkOpacity=0.05;", false));
        assert_eq!(note[1].fill, Fill::Shade(0.05));
        let s = style("shape=cube;boundedLbl=1;", false);
        assert_eq!(
            super::super::label_bounds("cube", R, &s, false),
            Rect::new(20.0, 20.0, 80.0, 30.0)
        );
    }

    #[test]
    fn a_tapes_bounded_label_keeps_between_its_waves() {
        let s = style("shape=tape;boundedLbl=1;", false);
        assert_eq!(tape_label(R, &s), Rect::new(0.0, 20.0, 100.0, 10.0));
        let s = style("shape=tape;boundedLbl=1;direction=south;", false);
        assert_eq!(tape_label(R, &s), Rect::new(40.0, 0.0, 20.0, 50.0));
    }

    #[test]
    fn the_general_palettes_outlines_fill_their_box() {
        for shape in [
            "document",
            "internalStorage",
            "folder",
            "component",
            "plus",
            "startState",
            "offPageConnector",
            "card",
            "tape",
            "or",
            "xor",
            "dataStorage",
            "message",
            "umlActor",
        ] {
            let parts = vertex(shape, R, &style(&format!("shape={shape};"), false));
            let body = path_bounds(&parts[0].path).unwrap();
            let all = parts
                .iter()
                .filter_map(|p| path_bounds(&p.path))
                .reduce(|a, b| a.union(&b))
                .unwrap();
            assert!(parts[0].fill == Fill::Cell, "{shape} has a body");
            assert!(R.grow(1e-9).contains_rect(&all), "{shape}: {all:?}");
            assert!(body.w > 0.0 && body.h > 0.0, "{shape}");
        }
    }

    #[test]
    fn a_partial_rectangle_strokes_only_the_sides_left_on() {
        let parts = vertex(
            "partialRectangle",
            R,
            &style("shape=partialRectangle;top=0;", false),
        );
        assert_eq!(parts[0].fill, Fill::Cell);
        assert!(!parts[0].stroke, "the fill is not outlined");
        let p = Point::new;
        assert_eq!(
            parts[1].path,
            [
                PathCmd::MoveTo(p(0.0, 0.0)),
                PathCmd::MoveTo(p(100.0, 0.0)),
                PathCmd::LineTo(p(100.0, 50.0)),
                PathCmd::LineTo(p(0.0, 50.0)),
                PathCmd::LineTo(p(0.0, 0.0)),
            ]
        );
        assert_eq!(
            parts[1].pen.cap,
            Cap::Square,
            "the sides meet on full corners"
        );
    }

    #[test]
    fn a_folders_tab_sits_right_or_left_and_its_label_below_it() {
        let tab = |s: &str| corners(&vertex("folder", R, &style(s, false))[0].path)[..4].to_vec();
        let p = Point::new;
        assert_eq!(
            tab("shape=folder;"),
            [p(40.0, 20.0), p(40.0, 0.0), p(100.0, 0.0), p(100.0, 20.0)]
        );
        assert_eq!(
            tab("shape=folder;tabPosition=left;tabWidth=30;tabHeight=10;"),
            [p(0.0, 10.0), p(0.0, 0.0), p(30.0, 0.0), p(30.0, 10.0)]
        );
        let s = style("shape=folder;boundedLbl=1;", false);
        assert_eq!(
            super::super::label_bounds("folder", R, &s, false),
            Rect::new(0.0, 15.0, 100.0, 35.0)
        );
    }

    #[test]
    fn a_waypoint_is_a_dot_in_the_stroke_colour() {
        let parts = vertex("waypoint", R, &style("shape=waypoint;size=6;", false));
        assert!(matches!(parts[0].fill, Fill::Own(_)));
        let dot = path_bounds(&parts[0].path).unwrap();
        // `size` less 2, and the stroke on either side.
        assert!(same_box(dot, Rect::new(47.0, 22.0, 6.0, 6.0)), "{dot:?}");
        assert_eq!(
            parts[1].fill,
            Fill::None,
            "an unpainted box that takes clicks"
        );
        let end = vertex("endState", R, &style("shape=endState;", false));
        assert_eq!(end.len(), 2, "a disc in a ring");
    }
}
