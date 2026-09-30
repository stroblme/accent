// The snapping here is derived from draw.io mxgraph/src/util/mxGuide.js and the size guides of
// js/grapheditor/Graph.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io
// AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! draw.io's guides. For a move (`mxGuide.move`): the moved box's sides and middle snap to those
//! of the other shapes and to the page's middle, and its gaps to the shapes before and after it
//! in a row or a column snap to the gap those keep between them. For a resize (the size guides,
//! Graph.js 27871-28476): the size a handle changes snaps to another shape's, and otherwise the
//! side it drags to another shape's side or middle. Each snap shows as a line, and an axis no
//! guide takes snaps to the grid. In page units, `px` being a screen pixel's worth.

use crate::geom::{Point, Rect, rotate};

/// A line to draw, in page units.
pub type Line = (Point, Point);

/// How near a side has to come to a guide off the grid, in page units (`mxGuide.tolerance`).
const TOLERANCE: f64 = 2.0;
/// How far an equal-distance guide keeps off the boxes it spans, and half its end ticks, in page
/// units (mxGuide.js 634).
const SHIFT: f64 = 5.0;

/// What a move snaps to besides the grid.
pub struct Targets<'a> {
    /// The other shapes' boxes.
    pub shapes: &'a [Rect],
    /// The page, whose middle lines guide too, ahead of any shape's (Graph.js 966-1022).
    pub page: Rect,
}

/// `delta`, a move of `bounds`, snapped to the guides, and the lines that show them. Within the
/// tolerance — half the grid, or [`TOLERANCE`] without one — equal distances snap first, then the
/// sides and middles, the equal distances winning where the two disagree.
pub fn snap(
    bounds: &Rect,
    delta: Point,
    targets: &Targets,
    grid: Option<f64>,
    px: f64,
) -> (Point, Vec<Line>) {
    let tolerance = grid.map_or(TOLERANCE, |g| g / 2.0).max(2.0 * px);
    let slack = (tolerance / 2.0).max(2.0 * px);
    let (equal, mut lines) = distances(targets.shapes, bounds, delta, slack, px);
    let starts = [bounds.x, bounds.y];
    let mut d = [equal[0].unwrap_or(delta.x), equal[1].unwrap_or(delta.y)];
    let b = bounds.translate(d[0], d[1]);
    let hits = [true, false].map(|horizontal| align(bounds, &b, targets, horizontal, tolerance));
    let mut aligned = [false; 2];
    for k in 0..2 {
        if let Some(hit) = &hits[k] {
            d[k] = hit.delta;
            aligned[k] = true;
        }
        if let Some(equal) = equal[k].filter(|e| d[k] != *e) {
            d[k] = equal;
            aligned[k] = false;
        }
        if let Some(g) = grid.filter(|_| !aligned[k] && equal[k].is_none()) {
            d[k] = snap_to(starts[k] + d[k], g) - starts[k];
        }
        // The page's middle may fall between two units: what lands on it is rounded to one
        // (`mxGuide.getDelta`).
        if hits[k].as_ref().is_some_and(|h| h.page) {
            d[k] = (starts[k] + d[k]).round() - starts[k];
        }
    }
    // Each guide runs across both boxes it lines up.
    for (k, hit) in hits.iter().enumerate() {
        let Some(hit) = hit.as_ref().filter(|_| aligned[k]) else {
            continue;
        };
        let horizontal = k == 0;
        let (other, other_len) = span(bounds, !horizontal);
        let (s, l) = span(&hit.by, !horizontal);
        let from = (other + d[1 - k]).min(s);
        let to = (other + other_len + d[1 - k]).max(s + l);
        lines.push(match horizontal {
            true => (Point::new(hit.at, from), Point::new(hit.at, to)),
            false => (Point::new(from, hit.at), Point::new(to, hit.at)),
        });
    }
    (Point::new(d[0], d[1]), lines)
}

fn snap_to(v: f64, grid: f64) -> f64 {
    (v / grid).round() * grid
}

/// Where a box starts along one axis (x when `horizontal`) and how long it is there.
fn span(r: &Rect, horizontal: bool) -> (f64, f64) {
    match horizontal {
        true => (r.x, r.w),
        false => (r.y, r.h),
    }
}

/// A side or the middle of the moved box on a guide.
struct Hit {
    /// The move along the axis that puts it there.
    delta: f64,
    /// Where the guide is along the axis.
    at: f64,
    /// The box the guide belongs to.
    by: Rect,
    /// Whether that box is the page.
    page: bool,
}

/// The nearest guide within `tolerance` of the moved box `b` along one axis: the middle to a
/// middle, a side to a side or a middle, and a side to the page's middle too. The first found
/// wins a tie, the page ahead of the shapes.
fn align(
    bounds: &Rect,
    b: &Rect,
    targets: &Targets,
    horizontal: bool,
    tolerance: f64,
) -> Option<Hit> {
    let (start, len) = span(bounds, horizontal);
    let (left, width) = span(b, horizontal);
    let (right, centre) = (left + width, left + width / 2.0);
    let mut tolerance = tolerance;
    let mut hit = None;
    let boxes =
        std::iter::once((targets.page, true)).chain(targets.shapes.iter().map(|r| (*r, false)));
    for (by, page) in boxes {
        let (s, l) = span(&by, horizontal);
        let mut guides = vec![(s + l / 2.0, true), (s, false), (s + l, false)];
        if page {
            guides.push((s + l / 2.0, false));
        }
        for (at, middle) in guides {
            let moved = match middle {
                true => [(centre, start + len / 2.0)]
                    .into_iter()
                    .find(|(v, _)| (at - v).abs() < tolerance),
                false => [(left, start), (right, start + len)]
                    .into_iter()
                    .find(|(v, _)| (at - v).abs() < tolerance),
            };
            if let Some((v, from)) = moved {
                tolerance = (at - v).abs();
                hit = Some(Hit {
                    delta: at - from,
                    at,
                    by,
                    page,
                });
            }
        }
    }
    hit
}

/// Equal distances (`mxGuide.moveDistance`): the moved box's gaps to the shapes beside it that
/// overlap it across the axis, snapped to the gap those keep between them. The move along x and
/// along y that does it, where one does, and the guides that show it.
fn distances(
    shapes: &[Rect],
    bounds: &Rect,
    delta: Point,
    tolerance: f64,
    px: f64,
) -> ([Option<f64>; 2], Vec<Line>) {
    let b = bounds.translate(delta.x, delta.y);
    // A gap of at least 4 px: three boxes on top of each other have no distance to show.
    let apart = 4.0 * px;
    let (mut row, mut column) = (Vec::new(), Vec::new());
    for s in shapes {
        let across_x = (b.x >= s.x && b.x <= s.right()) || (s.x >= b.x && s.x <= b.right());
        let across_y = (b.y >= s.y && b.y <= s.bottom()) || (s.y >= b.y && s.y <= b.bottom());
        if across_x && (b.y > s.bottom() + apart || b.bottom() + apart < s.y) {
            column.push(*s);
        } else if across_y && (b.x > s.right() + apart || b.right() + apart < s.x) {
            row.push(*s);
        }
    }
    let mut lines = Vec::new();
    let mut equal = [None, None];
    for (k, cells) in [row, column].into_iter().enumerate() {
        let horizontal = k == 0;
        if cells.len() > 1
            && let Some((at, guides)) = equal_gap(cells, &b, horizontal, tolerance)
        {
            equal[k] = Some(at - span(bounds, horizontal).0);
            lines.extend(guides);
        }
    }
    (equal, lines)
}

/// Where along one axis the moved box `b` starts when its gaps to the boxes in `cells` equal
/// theirs to each other (`mxGuide.snapDistance`), and the guides that show each gap: a line
/// [`SHIFT`] off either box with a tick at each end, beyond the first box that stays.
fn equal_gap(
    cells: Vec<Rect>,
    b: &Rect,
    horizontal: bool,
    tolerance: f64,
) -> Option<(f64, Vec<Line>)> {
    let pos = |c: &(Rect, bool)| span(&c.0, horizontal).0;
    let len = |c: &(Rect, bool)| span(&c.0, horizontal).1;
    let mut cells: Vec<(Rect, bool)> = cells.into_iter().map(|r| (r, false)).collect();
    cells.push((*b, true));
    // Stable, as the JavaScript sort is: the moved box stays after a box starting where it does.
    cells.sort_by(|a, c| pos(a).total_cmp(&pos(c)));
    let (first_moving, last_moving) = (cells[0].1, cells[cells.len() - 1].1);
    let (mut passed, mut count) = (false, 0);
    let (mut dist, mut fixed, mut mid) = (0.0, 0.0, 0.0);
    // Between two boxes, the space in the middle is the gap to snap to.
    if !first_moving
        && !last_moving
        && let Some(i) = (1..cells.len() - 1).find(|&i| cells[i].1)
    {
        mid = (pos(&cells[i + 1]) - pos(&cells[i - 1]) - len(&cells[i - 1]) - len(&cells[i])) / 2.0;
        dist = mid;
        fixed = dist;
    }
    let mut i = 0;
    while i + 1 < cells.len() {
        let (s1, s2) = (cells[i], cells[i + 1]);
        let moving = s1.1 || s2.1;
        let cur = pos(&s2) - pos(&s1) - len(&s1);
        passed = passed || s1.1;
        let mut next = i + 1;
        // A gap away from the moved box has to match exactly, save the second while the first
        // box moves and no gap is fixed yet.
        let slack = if moving || (i == 1 && passed) {
            tolerance
        } else {
            0.0
        };
        if dist == 0.0 && count == 0 {
            dist = cur;
            count = 1;
        } else if (dist - cur).abs() <= slack {
            count += 1;
        } else if count > 1 && passed {
            cells.truncate(i + 1);
            break;
        } else if cells.len() - i >= 3 && !passed {
            // Start counting again from here, the boxes before left out.
            count = 0;
            dist = mid;
            fixed = dist;
            cells.drain(0..i.max(1));
            next = 0;
        } else {
            break;
        }
        if fixed == 0.0 && !moving {
            fixed = cur;
            dist = fixed;
        }
        i = next;
    }
    // Three boxes with the moved one in the middle keep no gap: it goes to the middle.
    let mid_space = cells.len() == 3 && cells[1].1;
    if mid_space {
        fixed = 0.0;
    }
    // A fixed gap of 0 is no gap found: only the middle space may snap then (drawio#5722).
    if count < 2 || count != cells.len() - 1 || !(fixed > 0.0 || mid_space) {
        return None;
    }
    let first = cells[usize::from(cells[0].1)].0;
    let cross = match horizontal {
        true => first.bottom(),
        false => first.right(),
    };
    let mut result = 0.0;
    let mut points = Vec::new();
    if fixed > 0.0 {
        for w in cells.windows(2) {
            let (s1, s2) = (w[0], w[1]);
            if s1.1 {
                result = (pos(&s2) - len(&s1) - fixed).round();
                points.extend([result + len(&s1) + SHIFT, pos(&s2) - SHIFT]);
            } else if s2.1 {
                result = (pos(&s1) + len(&s1) + fixed).round();
                points.extend([pos(&s1) + len(&s1) + SHIFT, result - SHIFT]);
            } else {
                points.extend([pos(&s1) + len(&s1) + SHIFT, pos(&s2) - SHIFT]);
            }
        }
    } else {
        let (s1, moved, s3) = (cells[0], cells[1], cells[2]);
        let end = pos(&s1) + len(&s1);
        result = (end + (pos(&s3) - end - len(&moved)) / 2.0).round();
        points = vec![
            end + SHIFT,
            result - SHIFT,
            result + len(&moved) + SHIFT,
            pos(&s3) - SHIFT,
        ];
    }
    let at = |along: f64, off: f64| match horizontal {
        true => Point::new(along, cross + off),
        false => Point::new(cross + off, along),
    };
    let lines = points
        .chunks(2)
        .flat_map(|p| {
            [
                (at(p[0], 0.0), at(p[1], 0.0)),
                (at(p[0], -SHIFT), at(p[0], SHIFT)),
                (at(p[1], -SHIFT), at(p[1], SHIFT)),
            ]
        })
        .collect();
    Some((result, lines))
}

/// A shape a resize may take the size of, or line a side up with: its box as it shows, and the
/// turn the size mark on it takes (none for a quarter turn, whose box is already turned).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Neighbour {
    pub rect: Rect,
    pub turn: f64,
}

/// The sides of a box a resize handle drags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sides {
    pub left: bool,
    pub top: bool,
    pub right: bool,
    pub bottom: bool,
}

/// What a resize snapped to, for [`Resized::lines`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Resized {
    /// The shapes whose width and whose height the box took.
    width: Option<Neighbour>,
    height: Option<Neighbour>,
    /// A dragged side on another shape's side or middle, across x and across y: where, and that
    /// shape's box.
    edge_x: Option<(f64, Rect)>,
    edge_y: Option<(f64, Rect)>,
    /// The resized shape is turned a quarter, so its width is measured against their heights.
    swap: bool,
}

/// Snap `result`, `bounds` resized by dragging `sides` and in the shape's own frame (unturned),
/// to `shapes`, the nearest first (`mxVertexHandler.union` with the size guides): a size the
/// handle changes to the size of a shape within the tolerance (`snapSize`), and where no size
/// snapped, on an unturned shape, a dragged side to a shape's side or middle (`snapEdges`). The
/// side not dragged stays where it is.
pub fn snap_resize(
    result: &mut Rect,
    bounds: &Rect,
    sides: Sides,
    rotation: f64,
    shapes: &[Neighbour],
    grid: Option<f64>,
    px: f64,
) -> Resized {
    let tolerance = grid.map_or(TOLERANCE, |g| g / 2.0);
    let swap = rotation.rem_euclid(180.0) == 90.0;
    let mut snapped = Resized {
        swap,
        ..Resized::default()
    };
    // The size of the shape nearest in size within the tolerance, the nearest shape winning a tie
    // (`getSizeGuideMatch`).
    let size_match = |size: f64, width: bool| {
        let mut best: Option<(Neighbour, f64, f64)> = None;
        for n in shapes {
            let value = if width != swap { n.rect.w } else { n.rect.h };
            let diff = (value - size).abs();
            if value >= 1.0 && diff <= tolerance && best.is_none_or(|b| diff < b.2) {
                best = Some((*n, value, diff));
            }
        }
        best
    };
    // A changed width keeps the side not dragged: the left one unless the handle drags it.
    if (sides.left || sides.right)
        && let Some((n, width, _)) = size_match(result.w, true)
    {
        let kept = match sides.left {
            true => (result.right() - bounds.right()).abs() <= 0.01,
            false => (result.x - bounds.x).abs() <= 0.01,
        };
        if kept {
            if sides.left {
                result.x = result.right() - width;
            }
            result.w = width;
            snapped.width = Some(n);
        }
    }
    if (sides.top || sides.bottom)
        && let Some((n, height, _)) = size_match(result.h, false)
    {
        let kept = match sides.top {
            true => (result.bottom() - bounds.bottom()).abs() <= 0.01,
            false => (result.y - bounds.y).abs() <= 0.01,
        };
        if kept {
            if sides.top {
                result.y = result.bottom() - height;
            }
            result.h = height;
            snapped.height = Some(n);
        }
    }
    if rotation != 0.0 {
        return snapped;
    }
    // The side or middle of a shape nearest `value` within the tolerance (`getEdgeGuideMatch`).
    let tolerance = tolerance.max(2.0 * px);
    let edge_match = |value: f64, horizontal: bool| {
        let mut best: Option<(f64, Rect, f64)> = None;
        for n in shapes {
            let (s, l) = span(&n.rect, horizontal);
            for at in [s, s + l / 2.0, s + l] {
                let diff = (at - value).abs();
                if diff <= tolerance && best.is_none_or(|b| diff < b.2) {
                    best = Some((at, n.rect, diff));
                }
            }
        }
        best.map(|(at, by, _)| (at, by))
    };
    if snapped.width.is_none() {
        if sides.left && (result.right() - bounds.right()).abs() <= 0.01 {
            if let Some((at, by)) = edge_match(result.x, true)
                && result.right() - at >= 1.0
            {
                (result.w, result.x) = (result.right() - at, at);
                snapped.edge_x = Some((at, by));
            }
        } else if sides.right
            && (result.x - bounds.x).abs() <= 0.01
            && let Some((at, by)) = edge_match(result.right(), true)
            && at - result.x >= 1.0
        {
            result.w = at - result.x;
            snapped.edge_x = Some((at, by));
        }
    }
    if snapped.height.is_none() {
        if sides.top && (result.bottom() - bounds.bottom()).abs() <= 0.01 {
            if let Some((at, by)) = edge_match(result.y, false)
                && result.bottom() - at >= 1.0
            {
                (result.h, result.y) = (result.bottom() - at, at);
                snapped.edge_y = Some((at, by));
            }
        } else if sides.bottom
            && (result.y - bounds.y).abs() <= 0.01
            && let Some((at, by)) = edge_match(result.bottom(), false)
            && at - result.y >= 1.0
        {
            result.h = at - result.y;
            snapped.edge_y = Some((at, by));
        }
    }
    snapped
}

impl Resized {
    /// The lines that show the snaps, the resized box being `shown` turned `rotation`
    /// (`redrawSizeGuides`): a size taken marked across both boxes, and a side lined up with
    /// another shape's by a line along both.
    pub fn lines(&self, shown: &Rect, rotation: f64) -> Vec<Line> {
        let mut lines = Vec::new();
        if let Some(n) = &self.width {
            lines.extend(size_mark(shown, rotation, true));
            lines.extend(size_mark(&n.rect, n.turn, !self.swap));
        }
        if let Some(n) = &self.height {
            lines.extend(size_mark(shown, rotation, false));
            lines.extend(size_mark(&n.rect, n.turn, self.swap));
        }
        if let Some((x, by)) = &self.edge_x {
            let (top, bottom) = (shown.y.min(by.y), shown.bottom().max(by.bottom()));
            lines.push((Point::new(*x, top), Point::new(*x, bottom)));
        }
        if let Some((y, by)) = &self.edge_y {
            let (left, right) = (shown.x.min(by.x), shown.right().max(by.right()));
            lines.push((Point::new(left, *y), Point::new(right, *y)));
        }
        lines
    }
}

/// A box's width (or height) marked by a line through its middle with a tick at either end,
/// turned `turn` degrees about the middle (`createSizeGuideShape`).
fn size_mark(r: &Rect, turn: f64, horizontal: bool) -> [Line; 3] {
    let c = r.centre();
    let (p1, p2, d) = match horizontal {
        true => (
            Point::new(r.x, c.y),
            Point::new(r.right(), c.y),
            Point::new(0.0, SHIFT),
        ),
        false => (
            Point::new(c.x, r.y),
            Point::new(c.x, r.bottom()),
            Point::new(SHIFT, 0.0),
        ),
    };
    let at = |p: Point, k: f64| rotate(Point::new(p.x + k * d.x, p.y + k * d.y), c, turn);
    [
        (at(p1, 0.0), at(p2, 0.0)),
        (at(p1, -1.0), at(p1, 1.0)),
        (at(p2, -1.0), at(p2, 1.0)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 1000.0,
        h: 1000.0,
    };

    fn moved(
        bounds: Rect,
        delta: (f64, f64),
        shapes: &[Rect],
        grid: Option<f64>,
    ) -> (Point, Vec<Line>) {
        let targets = Targets { shapes, page: PAGE };
        snap(&bounds, Point::new(delta.0, delta.1), &targets, grid, 1.0)
    }

    #[test]
    fn a_side_near_another_shape_s_side_snaps_to_it_and_shows_the_line() {
        let other = Rect::new(100.0, 300.0, 80.0, 40.0);
        let (d, lines) = moved(
            Rect::new(0.0, 0.0, 40.0, 40.0),
            (103.0, 97.0),
            &[other],
            Some(10.0),
        );
        // The left side on the other's left, the top on the grid.
        assert_eq!(d, Point::new(100.0, 100.0));
        assert_eq!(
            lines,
            vec![(Point::new(100.0, 100.0), Point::new(100.0, 340.0))]
        );
        // Beyond half the grid, the grid alone.
        let (d, lines) = moved(
            Rect::new(0.0, 0.0, 40.0, 40.0),
            (93.0, 97.0),
            &[other],
            Some(10.0),
        );
        assert_eq!(d, Point::new(90.0, 100.0));
        assert!(lines.is_empty());
    }

    #[test]
    fn the_middle_snaps_to_the_page_s_middle() {
        let (d, lines) = moved(
            Rect::new(0.0, 0.0, 40.0, 40.0),
            (482.0, 13.0),
            &[],
            Some(10.0),
        );
        assert_eq!(d, Point::new(480.0, 10.0));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].0.x, 500.0);
    }

    #[test]
    fn a_gap_snaps_to_the_gap_of_the_two_before_it() {
        // Two boxes 30 apart in a row, the third dragged to 32 after the second.
        let row = [
            Rect::new(0.0, 0.0, 40.0, 40.0),
            Rect::new(70.0, 0.0, 40.0, 40.0),
        ];
        let (d, lines) = moved(
            Rect::new(142.0, 200.0, 40.0, 40.0),
            (0.0, -200.0),
            &row,
            None,
        );
        assert_eq!(d, Point::new(-2.0, -200.0), "its gap becomes 30");
        // A line with a tick at either end for each gap, along the first box's bottom.
        for gap in [(45.0, 65.0), (115.0, 135.0)] {
            let line = (Point::new(gap.0, 40.0), Point::new(gap.1, 40.0));
            assert!(lines.contains(&line), "{line:?} in {lines:?}");
        }
        let tick = (Point::new(45.0, 35.0), Point::new(45.0, 45.0));
        assert!(lines.contains(&tick));
    }

    #[test]
    fn between_two_shapes_a_box_snaps_to_the_middle() {
        let row = [
            Rect::new(0.0, 0.0, 40.0, 40.0),
            Rect::new(200.0, 0.0, 40.0, 40.0),
        ];
        let (d, _) = moved(
            Rect::new(0.0, 100.0, 40.0, 40.0),
            (99.0, -100.0),
            &row,
            None,
        );
        assert_eq!(d.x, 100.0, "80 on either side");
    }

    fn neighbour(x: f64, y: f64, w: f64, h: f64) -> Neighbour {
        Neighbour {
            rect: Rect::new(x, y, w, h),
            turn: 0.0,
        }
    }

    const RIGHT: Sides = Sides {
        left: false,
        top: false,
        right: true,
        bottom: false,
    };

    #[test]
    fn a_resize_takes_a_neighbour_s_size_and_marks_both() {
        let bounds = Rect::new(0.0, 0.0, 40.0, 40.0);
        let shapes = [neighbour(200.0, 0.0, 83.0, 40.0)];
        // Dragged to 80 wide on the grid; the neighbour's 83 is within half the grid.
        let mut r = Rect::new(0.0, 0.0, 80.0, 40.0);
        let snapped = snap_resize(&mut r, &bounds, RIGHT, 0.0, &shapes, Some(10.0), 1.0);
        assert_eq!(r, Rect::new(0.0, 0.0, 83.0, 40.0));
        let lines = snapped.lines(&r, 0.0);
        assert_eq!(lines.len(), 6, "a line and two ticks on each box");
        assert_eq!(lines[0], (Point::new(0.0, 20.0), Point::new(83.0, 20.0)));
        assert_eq!(lines[3], (Point::new(200.0, 20.0), Point::new(283.0, 20.0)));
    }

    #[test]
    fn a_dragged_side_lines_up_with_a_neighbour_s_middle() {
        let bounds = Rect::new(0.0, 0.0, 40.0, 40.0);
        let shapes = [neighbour(50.0, 100.0, 40.0, 40.0)];
        let mut r = Rect::new(0.0, 0.0, 72.0, 40.0);
        let snapped = snap_resize(&mut r, &bounds, RIGHT, 0.0, &shapes, Some(10.0), 1.0);
        assert_eq!(r.w, 70.0);
        let lines = snapped.lines(&r, 0.0);
        assert_eq!(
            lines,
            vec![(Point::new(70.0, 0.0), Point::new(70.0, 140.0))]
        );
        // A turned shape takes sizes only.
        let mut r = Rect::new(0.0, 0.0, 72.0, 40.0);
        let snapped = snap_resize(&mut r, &bounds, RIGHT, 30.0, &shapes, Some(10.0), 1.0);
        assert_eq!((r.w, snapped), (72.0, Resized::default()));
    }
}
