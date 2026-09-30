// Derived from draw.io js/grapheditor/Shapes.js and mxgraph/src/shape/mxArrowConnector.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! The shapes draw.io adds to mxGraph's (`Shapes.js`).

use super::{LINE_ARCSIZE, Part, add_points, quarter, rect};
use crate::geom::{PathCmd, Point, Rect};
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
/// 758-812): the outline, then the fold stroked over it.
// ponytail: `darkOpacity`, a translucent fill of the fold, is ignored; its default 0 draws none.
pub(super) fn note(b: Rect, style: &Resolved) -> Vec<Part> {
    let s = style.num("size", 30.0).min(b.h).min(b.w).max(0.0);
    let (w, h) = (b.w, b.h);
    let p = |x: f64, y: f64| Point::new(b.x + x, b.y + y);
    vec![
        Part::body(vec![
            PathCmd::MoveTo(p(0.0, 0.0)),
            PathCmd::LineTo(p(w - s, 0.0)),
            PathCmd::LineTo(p(w, s)),
            PathCmd::LineTo(p(w, h)),
            PathCmd::LineTo(p(0.0, h)),
            PathCmd::LineTo(p(0.0, 0.0)),
            PathCmd::Close,
        ]),
        Part::line(vec![
            PathCmd::MoveTo(p(w - s, 0.0)),
            PathCmd::LineTo(p(w - s, s)),
            PathCmd::LineTo(p(w, s)),
        ]),
    ]
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
        assert!(parts[0].fill && parts[0].stroke);
        assert!(!parts[1].fill && parts[1].stroke);
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
        assert!(!parts[1].fill && parts[1].stroke);
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
        assert!(!parts[0].fill && parts[0].stroke);
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
        assert!(parts[0].fill && parts[0].stroke);
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
}
