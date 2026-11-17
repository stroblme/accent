//! Keeping each group round what it holds (user decision 2026-11-17). A group that draws nothing
//! of its own fits its members exactly, growing and shrinking, a container only grows to hold
//! them, as draw.io's `mxGraph.extendParent` grows one, and a group with `transparentBounds=1`
//! keeps its geometry at 0,0,0,0 as draw.io does, its box being what it holds.

use std::collections::{HashMap, HashSet};

use super::{remove_with_edges, translate};
use crate::geom::Rect;
use crate::model::{Cell, CellId, Page};
use crate::style::Resolved;

/// Whether `cell` is a group with `transparentBounds=1` (`Graph.isTransparentBounds`).
pub(super) fn is_transparent(cell: &Cell) -> bool {
    cell.vertex && cell.style.get("transparentBounds") == Some("1")
}

/// The box vertex `cell` takes on the page, unturned: its rectangle, or for a group with
/// `transparentBounds=1` what it holds grown by its `groupPadding` (`Graph.getTransparentBounds`),
/// `None` while it holds nothing.
pub fn bounds(p: &Page, cell: &Cell) -> Option<Rect> {
    let r = p.absolute_rect(&cell.id)?;
    own_box(p, cell).map(|b| b.translate(r.x, r.y))
}

/// [`bounds`] in the cell's own frame, from its corner.
pub(super) fn own_box(p: &Page, cell: &Cell) -> Option<Rect> {
    let g = cell.geometry.as_ref()?;
    match is_transparent(cell) {
        true => transparent(p, cell, p.cells.len()),
        false => Some(Rect::new(0.0, 0.0, g.width, g.height)),
    }
}

/// Sibling bounds in parent coordinates (`Graph.getBoundsForGroup`): vertex rectangles, a group
/// with `transparentBounds=1` by what it holds, and edge endpoints and waypoints. Relative
/// vertices do not contribute.
pub(super) fn group_bounds(p: &Page, cells: &[CellId]) -> Option<Rect> {
    held(p, cells.iter().filter_map(|id| p.cell(id)), p.cells.len())
}

/// [`group_bounds`] of `cells`, going at most `depth` groups with `transparentBounds=1` deep,
/// which only a `parent` loop in a hand-edited file would reach.
fn held<'a>(p: &'a Page, cells: impl Iterator<Item = &'a Cell>, depth: usize) -> Option<Rect> {
    let mut bounds: Option<Rect> = None;
    let mut add = |r: Rect| bounds = Some(bounds.map_or(r, |b| b.union(&r)));
    for cell in cells {
        let Some(g) = &cell.geometry else { continue };
        if cell.edge {
            let loose = [
                g.source_point.filter(|_| cell.source.is_none()),
                g.target_point.filter(|_| cell.target.is_none()),
            ];
            let points = g.points.iter().flatten().copied();
            for pt in loose.into_iter().flatten().chain(points) {
                add(Rect::new(pt.x, pt.y, 0.0, 0.0));
            }
        } else if cell.vertex && !g.relative && !is_transparent(cell) {
            add(g.rect());
        } else if cell.vertex && !g.relative {
            let inner = depth.checked_sub(1).and_then(|d| transparent(p, cell, d));
            if let Some(r) = inner {
                add(r.translate(g.x, g.y));
            }
        }
    }
    bounds
}

/// The box group `cell` with `transparentBounds=1` shows in its own frame: what it holds, grown
/// by its `groupPadding`.
fn transparent(p: &Page, cell: &Cell, depth: usize) -> Option<Rect> {
    let inside = held(p, p.children(&cell.id), depth)?;
    Some(padded(inside, &cell.style.resolve(false)))
}

/// `r` grown by the room `style`'s `groupPadding` keeps round what a group holds, which draw.io
/// reads as CSS reads a padding, top, right, bottom and left (`mxUtils.parsePadding`).
fn padded(r: Rect, style: &Resolved) -> Rect {
    let [top, right, bottom, left] = padding(style);
    Rect::new(
        r.x - left,
        r.y - top,
        r.w + left + right,
        r.h + top + bottom,
    )
}

fn padding(style: &Resolved) -> [f64; 4] {
    let value = style.get("groupPadding").unwrap_or("0").replace("%20", " ");
    let n: Vec<f64> = value
        .split_whitespace()
        .map(|v| v.parse().unwrap_or(0.0))
        .collect();
    match n[..] {
        [] => [0.0; 4],
        [a] => [a; 4],
        [v, h] => [v, h, v, h],
        [t, h, b] => [t, h, b, h],
        [t, r, b, l, ..] => [t, r, b, l],
    }
}

/// The cells whose geometry a move of cell `id` translates: itself, or for a group with
/// `transparentBounds=1`, whose geometry stays at 0,0,0,0, what it holds (`Graph.translateCell`).
pub(super) fn moved_by(p: &Page, id: &str) -> Vec<CellId> {
    let (mut out, mut seen, mut next) = (Vec::new(), HashSet::new(), vec![id.to_string()]);
    while let Some(id) = next.pop() {
        match p.cell(&id) {
            Some(cell) if is_transparent(cell) && seen.insert(id.clone()) => {
                next.extend(p.children(&id).map(|c| c.id.clone()));
            }
            Some(_) => out.push(id),
            None => {}
        }
    }
    out
}

/// Move cell `id` by (`dx`, `dy`) in its parent, as draw.io's `Graph.translateCell` does.
pub(super) fn shift(p: &mut Page, id: &str, dx: f64, dy: f64) {
    for id in moved_by(p, id) {
        if let Some(g) = p.cell_mut(&id).and_then(|c| c.geometry.as_mut()) {
            translate(g, dx, dy);
        }
    }
}

/// Fit again each group an edit changed what it holds of, `before` being the page before the
/// edit and `after` the page it made: the parents of every cell put in, taken out, moved or
/// resized, innermost first, and theirs in turn as they change. A group that draws nothing of its
/// own and is no container fits its members' bounds, its origin moving and their offsets with it
/// so nothing moves on the page; any other shape only grows right and down to hold a member that
/// changed (`mxGraph.extendParent`), unless collapsed or `expand=0`; a group with
/// `transparentBounds=1` stays as it is. A group that draws nothing goes with the last of what it
/// held (`Graph.cellsRemoved`), and so may the one it was in. A table and its rows lay out their
/// cells themselves.
pub fn fit_groups(before: &Page, after: &mut Page) {
    let old: HashMap<&str, &Cell> = before.cells.iter().map(|c| (c.id.as_str(), c)).collect();
    let mut changed: HashSet<CellId> = HashSet::new();
    let mut groups: HashSet<CellId> = HashSet::new();
    for cell in &after.cells {
        let was = old.get(cell.id.as_str());
        if was.is_some_and(|w| w.geometry == cell.geometry && w.parent == cell.parent) {
            continue;
        }
        changed.insert(cell.id.clone());
        groups.extend(cell.parent.clone());
        groups.extend(was.and_then(|w| w.parent.clone()));
    }
    let now: HashSet<&str> = after.cells.iter().map(|c| c.id.as_str()).collect();
    let gone = before.cells.iter().filter(|c| !now.contains(c.id.as_str()));
    groups.extend(gone.filter_map(|c| c.parent.clone()));
    // Each cell's parent, read once, which a group that goes keeps here.
    let parents: HashMap<CellId, CellId> = (after.cells.iter())
        .filter_map(|c| Some((c.id.clone(), c.parent.clone()?)))
        .collect();
    let depth = |id: &str| {
        let mut at = parents.get(id);
        let mut n = 0;
        while let Some(parent) = at.filter(|_| n < parents.len()) {
            (n, at) = (n + 1, parents.get(parent));
        }
        n
    };
    while let Some(id) = groups.iter().max_by_key(|id| depth(id)).cloned() {
        groups.remove(&id);
        if fit(after, &id, &changed) {
            groups.extend(parents.get(&id).cloned());
            changed.insert(id);
        }
    }
}

/// Fit group `id` as [`fit_groups`] says; whether its place or size changed, or it went.
fn fit(p: &mut Page, id: &str, changed: &HashSet<CellId>) -> bool {
    let Some(cell) = p.cell(id).filter(|c| c.vertex) else {
        return false;
    };
    let Some(g) = cell.geometry.clone().filter(|g| !g.relative) else {
        return false;
    };
    let style = cell.style.resolve(false);
    let none = |key: &str| style.get(key).is_none_or(|v| v == "none");
    let bare = none("fillColor") && none("strokeColor") && style.get("image").is_none();
    let transparent = is_transparent(cell);
    let laid_out = style.get("childLayout").is_some() || style.shape() == "tableRow";
    if laid_out || cell.attr("collapsed") == Some("1") {
        return false;
    }
    let members: Vec<CellId> = p.children(id).map(|c| c.id.clone()).collect();
    if members.is_empty() {
        let empty = bare || transparent;
        if empty {
            remove_with_edges(p, HashSet::from([id.to_string()]));
        }
        return empty;
    }
    // Its box is what it holds, which has changed, so what holds it fits again.
    if transparent {
        return true;
    }
    if style.get("expand") == Some("0") {
        return false;
    }
    let fitted = match bare && !is_container(&style) {
        true => match group_bounds(p, &members) {
            Some(inside) => padded(inside, &style),
            None => return false,
        },
        false => {
            // Grown right and down, by the padding's right and bottom, to hold what changed.
            let [_, right, bottom, _] = padding(&style);
            let shapes = members.iter().filter(|m| changed.contains(*m));
            let shapes = shapes.filter(|m| p.cell(m).is_some_and(|c| c.vertex));
            let grown = shapes.filter_map(|m| group_bounds(p, std::slice::from_ref(m)));
            let (w, h) = grown.fold((g.width, g.height), |(w, h), r| {
                (w.max(r.right() + right), h.max(r.bottom() + bottom))
            });
            Rect::new(0.0, 0.0, w, h)
        }
    };
    if fitted == Rect::new(0.0, 0.0, g.width, g.height) {
        return false;
    }
    if let Some(geometry) = p.cell_mut(id).and_then(|c| c.geometry.as_mut()) {
        (geometry.x, geometry.y) = (g.x + fitted.x, g.y + fitted.y);
        (geometry.width, geometry.height) = (fitted.w, fitted.h);
    }
    // Its members are placed from its corner, which has moved.
    if (fitted.x, fitted.y) != (0.0, 0.0) {
        for member in &members {
            shift(p, member, -fitted.x, -fitted.y);
        }
    }
    true
}

/// Whether a shape of `style` holds its shapes as a container (`Graph.isContainer`): a swimlane
/// or a table unless `container=0`, anything else with `container=1`.
fn is_container(style: &Resolved) -> bool {
    match style.shape() {
        "swimlane" | "table" | "tableRow" => style.get("container") != Some("0"),
        _ => style.get("container") == Some("1"),
    }
}
