// Derived from draw.io src/main/webapp/mxgraph/src/view/mxEdgeStyle.js and src/main/webapp/mxgraph/src/util/mxUtils.js (Apache-2.0, Copyright (c) 2006-2026 JGraph Holdings Ltd / draw.io AG), ported to Rust and modified for accent; see crates/drawio/NOTICE.
//! The orthogonal router, `edgeStyle=orthogonalEdgeStyle`: draw.io's default, which leaves each
//! terminal by a side and joins the two by one of its route patterns.

use super::{State, Terminal, round_point, round_rect, round_tenth, segment_connector};
use crate::geom::{self, Point, Rect};
use crate::marker::DEFAULT_MARKERSIZE;
use crate::style::Resolved;

/// `mxEdgeStyle.orthBuffer`: the default jetty, the stub an orthogonal edge leaves a side with.
const ORTH_BUFFER: f64 = 10.0;

// mxConstants.DIRECTION_MASK_*: the sides an end may leave by.
const WEST: u32 = 1;
pub(super) const NORTH: u32 = 2;
pub(super) const SOUTH: u32 = 4;
pub(super) const EAST: u32 = 8;
const ALL: u32 = 15;

// mxEdgeStyle.LEFT/TOP/RIGHT/BOTTOM: the sides of a terminal's jetty limits.
const LEFT: usize = 1;
const TOP: usize = 2;
const RIGHT: usize = 4;
const BOTTOM: usize = 8;

// A route pattern step (mxEdgeStyle.js 931-958) holds a direction in its low four bits, then
// the side whose jetty limit it runs to, whether it runs to a centre instead, and whether that
// side or centre is the source's or the target's.
const SIDE_MASK: u32 = 480;
const CENTER_MASK: u32 = 512;
const SOURCE_MASK: u32 = 1024;
const TARGET_MASK: u32 = 2048;

/// Unit steps west, north, east and south (`mxEdgeStyle.dirVectors`, mxEdgeStyle.js 898).
const DIR_VECTORS: [[f64; 2]; 4] = [[-1.0, 0.0], [0.0, -1.0], [1.0, 0.0], [0.0, 1.0]];

/// The route for each pair of source and target sides, relative to the quadrant the target is
/// in (`mxEdgeStyle.routePatterns`, mxEdgeStyle.js 904-916).
const ROUTE_PATTERNS: [[&[u32]; 4]; 4] = [
    [
        &[513, 2308, 2081, 2562],
        &[513, 1090, 514, 2184, 2114, 2561],
        &[513, 1090, 514, 2564, 2184, 2562],
        &[513, 2308, 2561, 1090, 514, 2568, 2308],
    ],
    [
        &[514, 1057, 513, 2308, 2081, 2562],
        &[514, 2184, 2114, 2561],
        &[514, 2184, 2562, 1057, 513, 2564, 2184],
        &[514, 1057, 513, 2568, 2308, 2561],
    ],
    [
        &[1090, 514, 1057, 513, 2308, 2081, 2562],
        &[2114, 2561],
        &[1090, 2562, 1057, 513, 2564, 2184],
        &[1090, 514, 1057, 513, 2308, 2561, 2568],
    ],
    [
        &[2081, 2562],
        &[1057, 513, 1090, 514, 2184, 2114, 2561],
        &[1057, 513, 1090, 514, 2184, 2562, 2564],
        &[1057, 2561, 1090, 514, 2568, 2308],
    ],
];

/// The jetty of an end: `sourceJettySize`/`targetJettySize`, else `jettySize`, else 10. `auto`
/// makes room for the end's arrow.
// mxEdgeStyle.getJettySize, mxEdgeStyle.js 961-984
fn get_jetty_size(style: &Resolved, source: bool) -> f64 {
    let key = if source {
        "sourceJettySize"
    } else {
        "targetJettySize"
    };
    let key = if style.get(key).is_some() {
        key
    } else {
        "jettySize"
    };
    if style.get(key) != Some("auto") {
        return style.num(key, ORTH_BUFFER);
    }
    let (arrow, size) = if source {
        ("startArrow", "startSize")
    } else {
        ("endArrow", "endSize")
    };
    // Resolving drops an arrow set to `none`.
    if style.get(arrow).is_some() {
        let size = style.num(size, DEFAULT_MARKERSIZE);
        ((size + ORTH_BUFFER) / ORTH_BUFFER).ceil().max(2.0) * ORTH_BUFFER
    } else {
        2.0 * ORTH_BUFFER
    }
}

/// The sides named in a port constraint (`north`, `west`, `south`, `east`, as many as it holds).
pub(super) fn directions(value: &str) -> u32 {
    [
        ("north", NORTH),
        ("west", WEST),
        ("south", SOUTH),
        ("east", EAST),
    ]
    .into_iter()
    .filter(|(name, _)| value.contains(name))
    .fold(0, |mask, (_, side)| mask | side)
}

/// The sides an end may leave terminal `t` by: its `portConstraint`, else the edge's
/// `sourcePortConstraint` or `targetPortConstraint`, turned a quarter at a time with a terminal
/// that has `portConstraintRotation=1`; every side without either.
// mxUtils.getPortConstraints, mxUtils.js 3009-3127
pub(super) fn port_constraints(t: &Terminal, style: &Resolved, source: bool) -> u32 {
    let key = match source {
        true => "sourcePortConstraint",
        false => "targetPortConstraint",
    };
    let Some(mask) = t.port_constraint.or_else(|| style.get(key).map(directions)) else {
        return ALL;
    };
    let r = t.port_rotation;
    let quad = match () {
        _ if r >= 135.0 || r <= -135.0 => 2,
        _ if r > 45.0 => 1,
        _ if r < -45.0 => 3,
        _ => 0,
    };
    // Clockwise, each side moves `quad` places on.
    let clockwise = [NORTH, EAST, SOUTH, WEST];
    (0..4)
        .filter(|&i| mask & clockwise[i] != 0)
        .fold(0, |turned, i| turned | clockwise[(i + quad) % 4])
}

/// `mxUtils.reversePortConstraints`: west and east swapped, north and south swapped.
fn reverse_port_constraints(constraint: u32) -> u32 {
    ((constraint & WEST) << 3)
        | ((constraint & NORTH) << 1)
        | ((constraint & SOUTH) >> 1)
        | ((constraint & EAST) >> 3)
}

/// draw.io's default router. Each end leaves its terminal by a side, the one a fixed end sits
/// on or else one facing the other terminal, with a jetty; the route pattern for the pair of
/// sides then joins the jetties around both terminals. With waypoints, or fixed ends closer than
/// the two jetties, SegmentConnector routes instead.
// mxEdgeStyle.OrthConnector, mxEdgeStyle.js 1072-1646
pub(super) fn orth_connector(
    state: &State,
    source_scaled: Option<&Terminal>,
    target_scaled: Option<&Terminal>,
    control_hints: &[Point],
    result: &mut Vec<Option<Point>>,
) {
    let p0 = state.p0.map(round_point);
    let pe = state.pe.map(round_point);
    let source = source_scaled.map(|t| round_rect(t.bounds));
    let target = target_scaled.map(|t| round_rect(t.bounds));
    // A dangling end is a 1×1 box at its point.
    let Some(mut source_geo) = source.or(p0.map(|p| Rect::new(p.x, p.y, 1.0, 1.0))) else {
        return;
    };
    let Some(mut target_geo) = target.or(pe.map(|p| Rect::new(p.x, p.y, 1.0, 1.0))) else {
        return;
    };

    // The JS evens out the two jetties of a loop here, but tests `target == source` on two fresh
    // copies of the terminal, so draw.io never does, and neither does the port.
    let source_buffer = get_jetty_size(state.style, true);
    let target_buffer = get_jetty_size(state.style, false);
    let total_buffer = target_buffer + source_buffer;

    // Checks minimum distance for fixed points and falls back to segment connector
    let too_short = match (p0, pe) {
        (Some(a), Some(b)) => {
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            dx * dx + dy * dy < total_buffer * total_buffer
        }
        _ => false,
    };
    // `orthPointsFallback`
    if too_short || !control_hints.is_empty() {
        segment_connector(state, source_scaled, target_scaled, control_hints, result);
        return;
    }

    let port_constraint = [
        source_scaled.map_or(ALL, |t| port_constraints(t, state.style, true)),
        target_scaled.map_or(ALL, |t| port_constraints(t, state.style, false)),
    ];
    if let Some(t) = source_scaled
        && t.rotation != 0.0
    {
        source_geo = geom::bounding_box(&source_geo, t.rotation);
    }
    if let Some(t) = target_scaled
        && t.rotation != 0.0
    {
        target_geo = geom::bounding_box(&target_geo, t.rotation);
    }
    if source_geo.w == 0.0 || source_geo.h == 0.0 || target_geo.w == 0.0 || target_geo.h == 0.0 {
        return;
    }

    let mut dir = [0u32; 2];
    let geo = [source_geo, target_geo];
    let buffer = [source_buffer, target_buffer];
    // How far each side's jetty reaches, by side.
    let limits = [0, 1].map(|i| {
        let mut limit = [0.0; 9];
        limit[LEFT] = geo[i].x - buffer[i];
        limit[TOP] = geo[i].y - buffer[i];
        limit[RIGHT] = geo[i].right() + buffer[i];
        limit[BOTTOM] = geo[i].bottom() + buffer[i];
        limit
    });

    // Work out which quad the target is in
    let (sc, tc) = (geo[0].centre(), geo[1].centre());
    let (dx, dy) = (sc.x - tc.x, sc.y - tc.y);
    // 0 | 1
    // -----
    // 3 | 2
    let quad: u32 = if dx < 0.0 {
        if dy < 0.0 { 2 } else { 1 }
    } else if dy <= 0.0 {
        // Special case on x = 0 and negative y
        if dx == 0.0 { 2 } else { 3 }
    } else {
        0
    };

    // Check for connection constraints. An unattached end has no size, so the corner of its box
    // is exactly its point.
    let mut constraint = [[0.5, 0.5], [0.5, 0.5]];
    if source.is_none() {
        constraint[0] = [0.0, 0.0];
    }
    if target.is_none() {
        constraint[1] = [0.0, 0.0];
    }
    let fixed = [source.and(p0), target.and(pe)];
    for i in 0..2 {
        let Some(ct) = fixed[i] else { continue };
        let g = geo[i];
        constraint[i][0] = (ct.x - g.x) / g.w;
        if (ct.x - g.x).abs() <= 1.0 {
            dir[i] = WEST;
        } else if (ct.x - g.x - g.w).abs() <= 1.0 {
            dir[i] = EAST;
        }
        constraint[i][1] = (ct.y - g.y) / g.h;
        if (ct.y - g.y).abs() <= 1.0 {
            dir[i] = NORTH;
        } else if (ct.y - g.y - g.h).abs() <= 1.0 {
            dir[i] = SOUTH;
        }
    }

    let source_top_dist = geo[0].y - geo[1].bottom();
    let source_left_dist = geo[0].x - geo[1].right();
    let source_bottom_dist = geo[1].y - geo[0].bottom();
    let source_right_dist = geo[1].x - geo[0].right();
    // The room between the jetties, by direction index (1 west, 2 north, 3 east, 4 south).
    let vertex_separations = [
        0.0,
        (source_left_dist - total_buffer).max(0.0),
        (source_top_dist - total_buffer).max(0.0),
        (source_right_dist - total_buffer).max(0.0),
        (source_bottom_dist - total_buffer).max(0.0),
    ];

    // Start of source and target direction determination: the preferred and available sides of
    // each end, in order, from where the terminals are relative to each other.
    let mut hor_pref = [0u32; 2];
    let mut vert_pref = [0u32; 2];
    hor_pref[0] = if source_left_dist >= source_right_dist {
        WEST
    } else {
        EAST
    };
    vert_pref[0] = if source_top_dist >= source_bottom_dist {
        NORTH
    } else {
        SOUTH
    };
    hor_pref[1] = reverse_port_constraints(hor_pref[0]);
    vert_pref[1] = reverse_port_constraints(vert_pref[0]);
    let preferred_horiz_dist = source_left_dist.max(source_right_dist);
    let preferred_vert_dist = source_top_dist.max(source_bottom_dist);

    let mut pref_ordering = [[0u32; 2]; 2];
    let mut preferred_order_set = false;
    // If the preferred port isn't available, switch it
    for i in 0..2 {
        if dir[i] != 0 {
            continue;
        }
        if hor_pref[i] & port_constraint[i] == 0 {
            hor_pref[i] = reverse_port_constraints(hor_pref[i]);
        }
        if vert_pref[i] & port_constraint[i] == 0 {
            vert_pref[i] = reverse_port_constraints(vert_pref[i]);
        }
        pref_ordering[i] = [vert_pref[i], hor_pref[i]];
    }
    if preferred_vert_dist > 0.0 && preferred_horiz_dist > 0.0 {
        // Possibility of two segment edge connection
        if hor_pref[0] & port_constraint[0] > 0 && vert_pref[1] & port_constraint[1] > 0 {
            pref_ordering = [[hor_pref[0], vert_pref[0]], [vert_pref[1], hor_pref[1]]];
            preferred_order_set = true;
        } else if vert_pref[0] & port_constraint[0] > 0 && hor_pref[1] & port_constraint[1] > 0 {
            pref_ordering = [[vert_pref[0], hor_pref[0]], [hor_pref[1], vert_pref[1]]];
            preferred_order_set = true;
        }
    }
    if preferred_vert_dist > 0.0 && !preferred_order_set {
        pref_ordering = [[vert_pref[0], hor_pref[0]], [vert_pref[1], hor_pref[1]]];
        preferred_order_set = true;
    }
    if preferred_horiz_dist > 0.0 && !preferred_order_set {
        pref_ordering = [[hor_pref[0], vert_pref[0]], [hor_pref[1], vert_pref[1]]];
    }

    // The source and target prefs are now an ordered list of the preferred port selections.
    // If the list contains gaps, compact it.
    for i in 0..2 {
        if dir[i] != 0 {
            continue;
        }
        let pc = port_constraint[i];
        if pref_ordering[i][0] & pc == 0 {
            pref_ordering[i][0] = pref_ordering[i][1];
        }
        let mut dir_pref = pref_ordering[i][0] & pc;
        dir_pref |= (pref_ordering[i][1] & pc) << 8;
        dir_pref |= (pref_ordering[1 - i][i] & pc) << 16;
        dir_pref |= (pref_ordering[1 - i][1 - i] & pc) << 24;
        if dir_pref & 0xF == 0 {
            dir_pref <<= 8;
        }
        if dir_pref & 0xF00 == 0 {
            dir_pref = (dir_pref & 0xF) | (dir_pref >> 8);
        }
        if dir_pref & 0xF0000 == 0 {
            dir_pref = (dir_pref & 0xFFFF) | ((dir_pref & 0xF000000) >> 8);
        }
        dir[i] = dir_pref & 0xF;
        if [WEST, NORTH, EAST, SOUTH].contains(&pc) {
            dir[i] = pc;
        }
    }
    // End of source and target direction determination

    // Directions as indices 1 west, 2 north, 3 east, 4 south, which the quadrant turns.
    let index = |d: u32| (if d == EAST { 3 } else { d }) as i32;
    let quad_i = quad as i32;
    let relative = |d: u32| {
        let i = index(d) - quad_i;
        (if i < 1 { i + 4 } else { i }) as usize
    };
    let route_pattern = ROUTE_PATTERNS[relative(dir[0]) - 1][relative(dir[1]) - 1];

    let mut way_points = [[0.0; 2]; 12];
    way_points[0] = [geo[0].x, geo[0].y];
    match dir[0] {
        WEST => {
            way_points[0][0] -= source_buffer;
            way_points[0][1] += constraint[0][1] * geo[0].h;
        }
        SOUTH => {
            way_points[0][0] += constraint[0][0] * geo[0].w;
            way_points[0][1] += geo[0].h + source_buffer;
        }
        EAST => {
            way_points[0][0] += geo[0].w + source_buffer;
            way_points[0][1] += constraint[0][1] * geo[0].h;
        }
        NORTH => {
            way_points[0][0] += constraint[0][0] * geo[0].w;
            way_points[0][1] -= source_buffer;
        }
        _ => {}
    }

    let mut current_index = 0;
    // Orientation, 0 horizontal, 1 vertical
    let orientation = |d: u32| usize::from(d & (EAST | WEST) == 0);
    let mut last_orientation = orientation(dir[0]);
    let initial_orientation = last_orientation;

    for &step in route_pattern {
        // Rotate the index of this direction by the quad to get the real direction
        let mut direction_index = index(step & 0xF) + quad_i;
        if direction_index > 4 {
            direction_index -= 4;
        }
        let direction_index = direction_index as usize;
        let direction = DIR_VECTORS[direction_index - 1];
        let current_orientation = usize::from(direction_index.is_multiple_of(2));
        // Only update the current index if the point moved in the direction of the current
        // segment move, otherwise the same point is moved until there is a segment direction
        // change
        if current_orientation != last_orientation {
            current_index += 1;
            // Copy the previous way point into the new one. We can't base the new position on
            // index - 1 because sometime elbows turn out not to exist, then we'd have to rewind.
            way_points[current_index] = way_points[current_index - 1];
        }

        let tar = step & TARGET_MASK > 0;
        let sou = step & SOURCE_MASK > 0;
        let mut side = ((step & SIDE_MASK) >> 5) << quad;
        if side > 0xF {
            side >>= 4;
        }
        let center = step & CENTER_MASK > 0;

        if (sou || tar) && side < 9 {
            let sou_tar = if sou { 0 } else { 1 };
            let limit = if center && current_orientation == 0 {
                geo[sou_tar].x + constraint[sou_tar][0] * geo[sou_tar].w
            } else if center {
                geo[sou_tar].y + constraint[sou_tar][1] * geo[sou_tar].h
            } else {
                limits[sou_tar][side as usize]
            };
            let axis = current_orientation;
            let delta = (limit - way_points[current_index][axis]) * direction[axis];
            if delta > 0.0 {
                way_points[current_index][axis] += direction[axis] * delta;
            }
        } else if center {
            // Which center we're travelling to depend on the current direction
            let half = (vertex_separations[direction_index] / 2.0).abs();
            way_points[current_index][0] += direction[0] * half;
            way_points[current_index][1] += direction[1] * half;
        }

        if current_index > 0
            && way_points[current_index][current_orientation]
                == way_points[current_index - 1][current_orientation]
        {
            current_index -= 1;
        } else {
            last_orientation = current_orientation;
        }
    }

    for (i, wp) in way_points.iter().enumerate().take(current_index + 1) {
        if i == current_index {
            // Last point can cause last segment to be in same direction as jetty/approach. If
            // so, check the number of points is consistent with the relative orientation of
            // source and target jx. Same orientation requires an even number of turns (points),
            // different requires odd.
            let same_orient = usize::from(orientation(dir[1]) != initial_orientation);
            if same_orient != (current_index + 1) % 2 {
                // The last point isn't required
                break;
            }
        }
        result.push(Some(Point::new(round_tenth(wp[0]), round_tenth(wp[1]))));
    }

    // Removes duplicates
    let mut index = 1;
    while index < result.len() {
        match (result[index - 1], result[index]) {
            (Some(a), Some(b)) if a == b => {
                result.remove(index);
            }
            _ => index += 1,
        }
    }
}
