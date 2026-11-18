//! Keeping each group round what it holds (user decision 2026-11-17). A group that draws nothing
//! of its own fits its members exactly, growing and shrinking, a container only grows to hold
//! them, as draw.io's `mxGraph.extendParent` grows one, and a group with `transparentBounds=1`
//! keeps its geometry at 0,0,0,0 as draw.io does, its box being what it holds.

use std::collections::{BinaryHeap, HashMap, HashSet};

use super::{remove_with_edges, translate};
use crate::geom::Rect;
use crate::model::{Cell, CellId, Page};
use crate::style::Resolved;

/// How the walks below find a cell and what it holds: a page reading itself, for an edit that
/// looks up a few cells, or [`Indexed`], for one over a whole chain of groups, which a page would
/// read once a group.
pub(crate) trait Cells {
    fn cell(&self, id: &str) -> Option<&Cell>;
    fn children<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a Cell>;
}

impl Cells for Page {
    fn cell(&self, id: &str) -> Option<&Cell> {
        Page::cell(self, id)
    }

    fn children<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a Cell> {
        Page::children(self, id)
    }
}

/// Where each cell of a page is and what each holds, in document order, read once for the many
/// lookups of an edit over a chain of groups. It holds while no cell is put in or taken out.
struct Index {
    at: HashMap<CellId, usize>,
    held: HashMap<CellId, Vec<usize>>,
    count: usize,
}

impl Index {
    fn of(p: &Page) -> Index {
        let (mut at, mut held) = (HashMap::new(), HashMap::<CellId, Vec<usize>>::new());
        for (i, cell) in p.cells.iter().enumerate() {
            // The first of a duplicate id, as `Page::cell` finds it.
            at.entry(cell.id.clone()).or_insert(i);
            if let Some(parent) = &cell.parent {
                held.entry(parent.clone()).or_default().push(i);
            }
        }
        let count = p.cells.len();
        Index { at, held, count }
    }

    fn on<'a>(&'a self, page: &'a Page) -> Indexed<'a> {
        Indexed { page, index: self }
    }

    fn cell_mut<'p>(&self, p: &'p mut Page, id: &str) -> Option<&'p mut Cell> {
        self.at.get(id).map(|&i| &mut p.cells[i])
    }
}

/// A page looked up through its [`Index`].
struct Indexed<'a> {
    page: &'a Page,
    index: &'a Index,
}

impl Cells for Indexed<'_> {
    fn cell(&self, id: &str) -> Option<&Cell> {
        self.index.at.get(id).map(|&i| &self.page.cells[i])
    }

    fn children<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a Cell> {
        let held = self.index.held.get(id).into_iter().flatten();
        held.map(|&i| &self.page.cells[i])
    }
}

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
        // What it holds may be thousands of groups deep, each looked up once.
        true => transparent(&Index::of(p).on(p), cell),
        false => Some(Rect::new(0.0, 0.0, g.width, g.height)),
    }
}

/// Sibling bounds in parent coordinates (`Graph.getBoundsForGroup`): vertex rectangles, a group
/// with `transparentBounds=1` by what it holds, and edge endpoints and waypoints. Relative
/// vertices do not contribute.
pub(super) fn group_bounds(p: &impl Cells, cells: &[CellId]) -> Option<Rect> {
    let cells: Vec<&Cell> = cells.iter().filter_map(|id| p.cell(id)).collect();
    let nested = cells.iter().filter(|c| nests(c));
    let boxes = nested.filter_map(|c| Some((c.id.as_str(), transparent(p, c)?)));
    held(cells.iter().copied(), &boxes.collect())
}

/// Whether `cell` is a group with `transparentBounds=1` that what holds it measures by what it
/// holds in turn.
fn nests(cell: &Cell) -> bool {
    is_transparent(cell) && cell.geometry.as_ref().is_some_and(|g| !g.relative)
}

/// [`group_bounds`] of `cells`, those with `transparentBounds=1` by their `boxes`.
fn held<'a>(cells: impl Iterator<Item = &'a Cell>, boxes: &HashMap<&str, Rect>) -> Option<Rect> {
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
        } else if let Some(r) = boxes.get(cell.id.as_str()).filter(|_| nests(cell)) {
            add(r.translate(g.x, g.y));
        }
    }
    bounds
}

/// The box group `cell` with `transparentBounds=1` shows in its own frame: what it holds, grown
/// by its `groupPadding`. The groups with `transparentBounds=1` it holds are measured first,
/// innermost first, from a list instead of a recursion, as a file bounds no chain of them, and
/// each once, so that a `parent` loop ends.
fn transparent(p: &impl Cells, cell: &Cell) -> Option<Rect> {
    // Each group after every one it holds, once the list is turned round.
    let (mut order, mut next, mut seen) = (Vec::new(), vec![cell], HashSet::new());
    while let Some(group) = next.pop() {
        if seen.insert(group.id.as_str()) {
            order.push(group);
            next.extend(p.children(&group.id).filter(|c| nests(c)));
        }
    }
    let mut boxes: HashMap<&str, Rect> = HashMap::new();
    for group in order.into_iter().rev() {
        if let Some(inside) = held(p.children(&group.id), &boxes) {
            let style = group.style.resolve(false);
            boxes.insert(group.id.as_str(), padded(inside, &style));
        }
    }
    boxes.get(cell.id.as_str()).copied()
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
pub(crate) fn moved_by(p: &impl Cells, id: &str) -> Vec<CellId> {
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
    for id in moved_by(&*p, id) {
        if let Some(g) = p.cell_mut(&id).and_then(|c| c.geometry.as_mut()) {
            translate(g, dx, dy);
        }
    }
}

/// [`shift`] through `index`.
fn shift_in(p: &mut Page, index: &Index, id: &str, dx: f64, dy: f64) {
    for id in moved_by(&index.on(p), id) {
        if let Some(g) = index.cell_mut(p, &id).and_then(|c| c.geometry.as_mut()) {
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
/// held (`Graph.cellsRemoved`), and so may the one it was in. A table, a stack and what they lay
/// out (`childLayout`) are left to their layout.
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
    let mut index = Index::of(after);
    let depths = depths(after, &index);
    let depth = |id: &CellId| depths.get(id).copied().unwrap_or(0);
    // The deepest first, each once.
    let mut next: BinaryHeap<(usize, CellId)> =
        groups.iter().map(|g| (depth(g), g.clone())).collect();
    while let Some((_, id)) = next.pop() {
        if fit(after, &index, &id, &changed) {
            let parent = parents.get(&id).filter(|p| groups.insert((*p).clone()));
            next.extend(parent.map(|p| (depth(p), p.clone())));
            changed.insert(id);
        }
        // A group that went took its place, and maybe arrows, with it.
        if after.cells.len() != index.count {
            index = Index::of(after);
        }
    }
}

/// How deep each cell of `p` is below its root, walked down from the roots, so that a cell in a
/// `parent` loop, which no root holds, has none.
fn depths(p: &Page, index: &Index) -> HashMap<CellId, usize> {
    let roots = p.cells.iter().filter(|c| c.parent.is_none());
    let mut next: Vec<(&Cell, usize)> = roots.map(|c| (c, 0)).collect();
    let mut depths = HashMap::new();
    let page = index.on(p);
    while let Some((cell, depth)) = next.pop() {
        if depths.insert(cell.id.clone(), depth).is_none() {
            next.extend(page.children(&cell.id).map(|c| (c, depth + 1)));
        }
    }
    depths
}

/// Fit group `id` as [`fit_groups`] says, looking cells up through `index`; whether its place or
/// size changed, or it went.
fn fit(p: &mut Page, index: &Index, id: &str, changed: &HashSet<CellId>) -> bool {
    let page = index.on(p);
    let Some(cell) = page.cell(id).filter(|c| c.vertex) else {
        return false;
    };
    let Some(g) = cell.geometry.clone().filter(|g| !g.relative) else {
        return false;
    };
    let style = cell.style.resolve(false);
    let none = |key: &str| style.get(key).is_none_or(|v| v == "none");
    let bare = none("fillColor") && none("strokeColor") && style.get("image").is_none();
    let transparent = is_transparent(cell);
    // A table, a stack and what they lay out are placed by the layout.
    let parent = cell.parent.as_deref().and_then(|id| page.cell(id));
    let in_layout = parent.is_some_and(|c| c.style.resolve(false).get("childLayout").is_some());
    let laid_out = in_layout || style.get("childLayout").is_some();
    if laid_out || cell.attr("collapsed") == Some("1") {
        return false;
    }
    let members: Vec<CellId> = page.children(id).map(|c| c.id.clone()).collect();
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
        true => match group_bounds(&page, &members) {
            Some(inside) => padded(inside, &style),
            None => return false,
        },
        false => {
            // Grown right and down, by the padding's right and bottom, to hold what changed.
            let [_, right, bottom, _] = padding(&style);
            let shapes = members.iter().filter(|m| changed.contains(*m));
            let shapes = shapes.filter(|m| page.cell(m).is_some_and(|c| c.vertex));
            let grown = shapes.filter_map(|m| group_bounds(&page, std::slice::from_ref(m)));
            let (w, h) = grown.fold((g.width, g.height), |(w, h), r| {
                (w.max(r.right() + right), h.max(r.bottom() + bottom))
            });
            Rect::new(0.0, 0.0, w, h)
        }
    };
    // Within float dust of where it is, as a group scaled with what it holds lands.
    let moved = [fitted.x, fitted.y, fitted.w - g.width, fitted.h - g.height];
    if moved.iter().all(|v| v.abs() < 1e-6) {
        return false;
    }
    if let Some(geometry) = index.cell_mut(p, id).and_then(|c| c.geometry.as_mut()) {
        (geometry.x, geometry.y) = (g.x + fitted.x, g.y + fitted.y);
        (geometry.width, geometry.height) = (fitted.w, fitted.h);
    }
    // Its members are placed from its corner, which has moved.
    if (fitted.x, fitted.y) != (0.0, 0.0) {
        for member in &members {
            shift_in(p, index, member, -fitted.x, -fitted.y);
        }
    }
    true
}

/// Whether resizing vertex `cell` scales what it holds with it, as draw.io's handles and Arrange
/// panel do (`Graph.isRecursiveVertexResize`): a shape holding cells, but not a swimlane, one
/// collapsed, one laid out by a `childLayout` or one with `recursiveResize=0`.
pub(super) fn scales_children(p: &Page, cell: &Cell) -> bool {
    let style = cell.style.resolve(false);
    cell.vertex
        && p.children(&cell.id).next().is_some()
        && style.shape() != "swimlane"
        && cell.attr("collapsed") != Some("1")
        && style.get("recursiveResize") != Some("0")
        && style.get("childLayout").is_none()
}

/// Scale what vertex `id` holds by `sx` across and `sy` down from its corner, as draw.io's
/// `mxGraph.resizeChildCells` does (`mxGraph.scaleCell`, `mxGeometry.scale`): each shape's place
/// and size, by the smaller factor for one with `aspect=fixed`, a line's waypoints and loose
/// ends, and what each shape holds by its own change in turn. `resizeWidth` and `resizeHeight`
/// scale a side or keep it, and a shape locked or with `movable=0` keeps its place, with
/// `resizable=0` its size. What a group with `transparentBounds=1` holds is scaled with its
/// group's other cells, the group keeping its 0,0,0,0, where draw.io leaves it as it is.
pub(super) fn scale_children(p: &mut Page, id: &str, sx: f64, sy: f64) {
    // A list of the shapes still to scale instead of a recursion, as a file bounds no chain of
    // parents, each shape scaled once so that a `parent` loop ends.
    let (mut next, mut seen) = (vec![(id.to_string(), sx, sy)], HashSet::new());
    let index = Index::of(p);
    while let Some((id, sx, sy)) = next.pop() {
        if seen.insert(id.clone()) {
            scale_held(p, &index, &id, (sx, sy), &mut next);
        }
    }
}

/// One step of [`scale_children`], scaling what `id` holds by `(sx, sy)` and putting each shape
/// whose size changed on `next` with its own factors.
fn scale_held(p: &mut Page, index: &Index, id: &str, by: (f64, f64), next: &mut Vec<Scale>) {
    let (sx, sy) = by;
    let page = index.on(p);
    let held = page.children(id).flat_map(|c| moved_by(&page, &c.id));
    let held: Vec<CellId> = held.collect();
    for child in held {
        let page = index.on(p);
        let Some(cell) = page.cell(&child) else {
            continue;
        };
        let Some(g) = cell.geometry.clone() else {
            continue;
        };
        let style = cell.style.resolve(cell.edge);
        let says = |key: &str, value: &str| style.get(key) == Some(value);
        let locked = says("locked", "1");
        let mut s = g.clone();
        let ends = s.source_point.iter_mut().chain(&mut s.target_point);
        for pt in ends.chain(s.points.iter_mut().flatten()) {
            (pt.x, pt.y) = (pt.x * sx, pt.y * sy);
        }
        if !g.relative {
            let (fx, fy) = match says("aspect", "fixed") {
                true => (sx.min(sy), sx.min(sy)),
                false => (sx, sy),
            };
            (s.x, s.y, s.width, s.height) = (g.x * sx, g.y * sy, g.width * fx, g.height * fy);
        }
        match style.get("resizeWidth") {
            Some("1") => s.width = g.width * sx,
            Some("0") => s.width = g.width,
            _ => {}
        }
        match style.get("resizeHeight") {
            Some("1") => s.height = g.height * sy,
            Some("0") => s.height = g.height,
            _ => {}
        }
        if locked || says("movable", "0") {
            (s.x, s.y) = (g.x, g.y);
        }
        if locked || says("resizable", "0") {
            (s.width, s.height) = (g.width, g.height);
        }
        let ratio = |new: f64, old: f64| if old != 0.0 { new / old } else { 1.0 };
        let inner = (ratio(s.width, g.width), ratio(s.height, g.height));
        let vertex = cell.vertex;
        if let Some(c) = index.cell_mut(p, &child) {
            c.geometry = Some(s);
        }
        if vertex && inner != (1.0, 1.0) {
            next.push((child, inner.0, inner.1));
        }
    }
}

/// A shape still to scale, by its factors across and down.
type Scale = (CellId, f64, f64);

/// Whether a shape of `style` holds its shapes as a container (`Graph.isContainer`): a swimlane
/// or a table unless `container=0`, anything else with `container=1`.
fn is_container(style: &Resolved) -> bool {
    match style.shape() {
        "swimlane" | "table" | "tableRow" => style.get("container") != Some("0"),
        _ => style.get("container") == Some("1"),
    }
}
