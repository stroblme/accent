//! The canvas's arithmetic, free of GTK so it is tested without a display: where a page point
//! lands in the scrolled content, what fitting means, snapping to the grid, the selection's
//! handles, and which cell a click selects.

use std::collections::{HashMap, HashSet};

use accent_drawio::geom::rotate;
use accent_drawio::guide::Neighbour;
use accent_drawio::handle::{self, Kind, Knob, Terminal};
use accent_drawio::{CellId, Constraint, Context, Page, PathCmd, Point, Prim, Rect, Scene};

pub const MIN_SCALE: f64 = 0.1;
pub const MAX_SCALE: f64 = 8.0;
/// Room left around the drawing, in pixels on screen.
pub const GAP: f64 = 24.0;
/// A selection handle's side, in pixels on screen — the PDF's Adjust box's.
pub const HANDLE: f64 = 8.0;
/// A press has to move this far, in pixels, before it is a drag rather than a click.
pub const DRAG_SLOP: f64 = 3.0;
/// How close to a line a click has to land to take it, in pixels on screen.
pub const TOLERANCE: f64 = 4.0;
/// A new shape dropped with a click rather than drawn: draw.io's own default vertex.
pub const DEFAULT_SIZE: (f64, f64) = (120.0, 60.0);
/// How far beyond a selected shape's top-right corner its rotate handle sits, in pixels:
/// draw.io's `rotationHandleVSpacing` (Graph.js 25334).
pub const ROTATE_GAP: f64 = 12.0;
/// The smallest a shape can be resized to, in page units.
pub const MIN_SIZE: f64 = 10.0;

/// How big the page is drawn: fitted to the window, or at a chosen scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Zoom {
    Fit,
    Scale(f64),
}

/// Where page coordinates land in the scrolled content: `content = page · scale + (x, y)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub scale: f64,
    pub x: f64,
    pub y: f64,
}

impl Default for Frame {
    fn default() -> Frame {
        Frame {
            scale: 1.0,
            x: 0.0,
            y: 0.0,
        }
    }
}

impl Frame {
    pub fn to_content(self, p: Point) -> Point {
        Point::new(self.x + p.x * self.scale, self.y + p.y * self.scale)
    }

    pub fn to_page(self, c: Point) -> Point {
        Point::new((c.x - self.x) / self.scale, (c.y - self.y) / self.scale)
    }

    /// A page rectangle in content coordinates.
    pub fn rect(self, r: &Rect) -> Rect {
        let p = self.to_content(Point::new(r.x, r.y));
        Rect::new(p.x, p.y, r.w * self.scale, r.h * self.scale)
    }
}

/// The frame and the content size for a drawing covering `extent` (page units) at `scale` in a
/// viewport: the drawing with [`GAP`] around it, centred while it is smaller than the viewport.
pub fn frame(extent: Rect, scale: f64, viewport: (f64, f64)) -> (Frame, (f64, f64)) {
    let (dw, dh) = (extent.w * scale, extent.h * scale);
    let size = (
        (dw + 2.0 * GAP).max(viewport.0),
        (dh + 2.0 * GAP).max(viewport.1),
    );
    let frame = Frame {
        scale,
        x: (size.0 - dw) / 2.0 - extent.x * scale,
        y: (size.1 - dh) / 2.0 - extent.y * scale,
    };
    (frame, size)
}

/// The scale that shows the whole page in `viewport`, [`GAP`] around it.
pub fn fit_scale(page: (f64, f64), viewport: (f64, f64)) -> f64 {
    let fit = ((viewport.0 - 2.0 * GAP) / page.0.max(1.0))
        .min((viewport.1 - 2.0 * GAP) / page.1.max(1.0));
    clamp_scale(fit)
}

pub fn clamp_scale(scale: f64) -> f64 {
    if scale.is_finite() {
        scale.clamp(MIN_SCALE, MAX_SCALE)
    } else {
        1.0
    }
}

pub fn snap(v: f64, grid: f64) -> f64 {
    (v / grid).round() * grid
}

/// A move of `delta` corrected so that `origin` lands on the grid, as draw.io snaps a drag.
pub fn snap_move(origin: Point, delta: Point, grid: f64) -> Point {
    Point::new(
        snap(origin.x + delta.x, grid) - origin.x,
        snap(origin.y + delta.y, grid) - origin.y,
    )
}

/// [`resize_by`] for a shape turned `rotation` degrees: the pointer's move is taken into the
/// shape's own frame, the box resized there, and the result placed so that the side the handle
/// does not hold stays where it was on the page — draw.io's rule. On the grid, a turned shape's
/// size snaps rather than its edges, which the placing moves off the grid anyway
/// (`mxVertexHandler.union`, 1703-1709). `snap` then has the box in the shape's own frame, `r`
/// beside it, before it is placed: the size guides, which work there as draw.io's do.
pub fn resize_rotated(
    r: &Rect,
    rotation: f64,
    handle: Handle,
    delta: Point,
    grid: Option<f64>,
    snap: impl FnOnce(&mut Rect, &Rect),
) -> Rect {
    if rotation == 0.0 {
        let mut out = resize_by(r, handle, delta, grid);
        snap(&mut out, r);
        return out;
    }
    let origin = Point::default();
    let local = resize_by(r, handle, rotate(delta, origin, -rotation), None);
    let mut local = grid.map_or(local, |g| snap_size(&local, handle, g));
    snap(&mut local, r);
    let (old, new) = (r.centre(), local.centre());
    let shift = rotate(Point::new(new.x - old.x, new.y - old.y), origin, rotation);
    let centre = Point::new(old.x + shift.x, old.y + shift.y);
    Rect::new(
        centre.x - local.w / 2.0,
        centre.y - local.h / 2.0,
        local.w,
        local.h,
    )
}

/// `r` with the sides `handle` moves placed so its size is on the grid, the other sides kept.
fn snap_size(r: &Rect, handle: Handle, grid: f64) -> Rect {
    let (left, top, right, bottom) = handle.edges();
    let size = |v: f64| snap(v, grid).max(MIN_SIZE);
    let (mut l, mut t, mut rr, mut b) = (r.x, r.y, r.right(), r.bottom());
    if right {
        rr = l + size(r.w);
    } else if left {
        l = rr - size(r.w);
    }
    if bottom {
        b = t + size(r.h);
    } else if top {
        t = b - size(r.h);
    }
    Rect::new(l, t, rr - l, b - t)
}

/// Where the rotate handle of a shape drawn at `r` (content coordinates) sits before its turn:
/// beyond the top-right corner, where draw.io puts it (Graph.js 25335-25341).
pub fn rotate_handle(r: &Rect) -> Point {
    Point::new(r.right() + ROTATE_GAP, r.y - ROTATE_GAP)
}

/// The compass bearing of `p` from `centre`, in degrees clockwise from straight up.
fn bearing(centre: Point, p: Point) -> f64 {
    (p.x - centre.x).atan2(centre.y - p.y).to_degrees()
}

/// The turn a shape centred on `centre` takes when its rotate handle, at `handle` before the
/// turn, is dragged to `pointer` (all in content coordinates): the pointer's bearing less the
/// handle's, as `mxVertexHandler.rotateVertex` (953-992) has it. On the grid it snaps to 15°
/// while the pointer is near the handle's circle, to 5° a little outside it and to 1° further
/// out; `free`, to a tenth. Kept within (-180, 180].
pub fn rotation_to(centre: Point, handle: Point, pointer: Point, free: bool) -> f64 {
    let alpha = bearing(centre, pointer) - bearing(centre, handle);
    let raster = match pointer.distance(centre) - handle.distance(centre) {
        _ if free => 0.1,
        out if out < 2.0 => 15.0,
        out if out < 25.0 => 5.0,
        _ => 1.0,
    };
    let turn = ((alpha / raster).round() * raster).rem_euclid(360.0);
    let turn = if turn > 180.0 { turn - 360.0 } else { turn };
    // A tenth is as fine as it goes; rounding there keeps float dust out of the file.
    (turn * 10.0).round() / 10.0
}

/// The point halfway along `path` by length: where draw.io puts an edge's label. A curve is
/// taken as the line to its end.
pub fn midpoint(path: &[PathCmd]) -> Option<Point> {
    let points: Vec<Point> = path
        .iter()
        .filter_map(|c| match c {
            PathCmd::MoveTo(p)
            | PathCmd::LineTo(p)
            | PathCmd::QuadTo(_, p)
            | PathCmd::CurveTo(_, _, p) => Some(*p),
            PathCmd::Close => None,
        })
        .collect();
    let mut left = points.windows(2).map(|w| w[0].distance(w[1])).sum::<f64>() / 2.0;
    for w in points.windows(2) {
        let d = w[0].distance(w[1]);
        if d > 0.0 && d >= left {
            let t = left / d;
            return Some(Point::new(
                w[0].x + (w[1].x - w[0].x) * t,
                w[0].y + (w[1].y - w[0].y) * t,
            ));
        }
        left -= d;
    }
    points.first().copied()
}

/// One of the eight handles on a selected shape's box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    NorthWest,
    North,
    NorthEast,
    West,
    East,
    SouthWest,
    South,
    SouthEast,
}

impl Handle {
    pub const ALL: [Handle; 8] = [
        Handle::NorthWest,
        Handle::North,
        Handle::NorthEast,
        Handle::West,
        Handle::East,
        Handle::SouthWest,
        Handle::South,
        Handle::SouthEast,
    ];

    /// Where on `r` the handle sits.
    pub fn at(self, r: &Rect) -> Point {
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        match self {
            Handle::NorthWest => Point::new(r.x, r.y),
            Handle::North => Point::new(cx, r.y),
            Handle::NorthEast => Point::new(r.right(), r.y),
            Handle::West => Point::new(r.x, cy),
            Handle::East => Point::new(r.right(), cy),
            Handle::SouthWest => Point::new(r.x, r.bottom()),
            Handle::South => Point::new(cx, r.bottom()),
            Handle::SouthEast => Point::new(r.right(), r.bottom()),
        }
    }

    /// The handles in compass order, clockwise from the top.
    const COMPASS: [Handle; 8] = [
        Handle::North,
        Handle::NorthEast,
        Handle::East,
        Handle::SouthEast,
        Handle::South,
        Handle::SouthWest,
        Handle::West,
        Handle::NorthWest,
    ];

    /// The handle that points the way this one does once its shape is turned `rotation`
    /// degrees: whose resize cursor to show over it.
    pub fn turned(self, rotation: f64) -> Handle {
        let at = Handle::COMPASS.iter().position(|h| *h == self).unwrap_or(0);
        let steps = (rotation / 45.0).round() as i64;
        Handle::COMPASS[(at as i64 + steps).rem_euclid(8) as usize]
    }

    /// The pointer a handle shows, by its CSS cursor name.
    pub fn cursor(self) -> &'static str {
        match self {
            Handle::NorthWest => "nw-resize",
            Handle::North => "n-resize",
            Handle::NorthEast => "ne-resize",
            Handle::West => "w-resize",
            Handle::East => "e-resize",
            Handle::SouthWest => "sw-resize",
            Handle::South => "s-resize",
            Handle::SouthEast => "se-resize",
        }
    }

    /// The sides the handle drags, as the guides take them.
    pub fn sides(self) -> accent_drawio::guide::Sides {
        let (left, top, right, bottom) = self.edges();
        accent_drawio::guide::Sides {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Which of the four edges the handle moves: (left, top, right, bottom).
    fn edges(self) -> (bool, bool, bool, bool) {
        match self {
            Handle::NorthWest => (true, true, false, false),
            Handle::North => (false, true, false, false),
            Handle::NorthEast => (false, true, true, false),
            Handle::West => (true, false, false, false),
            Handle::East => (false, false, true, false),
            Handle::SouthWest => (true, false, false, true),
            Handle::South => (false, false, false, true),
            Handle::SouthEast => (false, false, true, true),
        }
    }
}

/// The handle of `r` (content coordinates) under `at`, if any.
pub fn handle_at(r: &Rect, at: Point, grip: f64) -> Option<Handle> {
    Handle::ALL
        .into_iter()
        .find(|h| h.at(r).distance(at) <= grip)
}

/// `r` with the edges `handle` holds moved by `delta`, snapped to `grid` when there is one, and
/// never smaller than [`MIN_SIZE`]. Each axis on its own, as draw.io resizes.
pub fn resize_by(r: &Rect, handle: Handle, delta: Point, grid: Option<f64>) -> Rect {
    let place = |v: f64| grid.map_or(v, |g| snap(v, g));
    let (left, top, right, bottom) = handle.edges();
    let (mut l, mut t, mut rr, mut b) = (r.x, r.y, r.right(), r.bottom());
    if left {
        l = place(l + delta.x).min(rr - MIN_SIZE);
    }
    if right {
        rr = place(rr + delta.x).max(l + MIN_SIZE);
    }
    if top {
        t = place(t + delta.y).min(b - MIN_SIZE);
    }
    if bottom {
        b = place(b + delta.y).max(t + MIN_SIZE);
    }
    Rect::new(l, t, rr - l, b - t)
}

/// One end of a connector being drawn: the shape it attaches to, if any, where it is, and the
/// connection point that pins it when it was dropped on one.
pub type End = (Option<CellId>, Point, Option<Constraint>);

/// What a press landed on: see [`Sheet::pick`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    pub cell: CellId,
    pub held: Option<CellId>,
}

/// One page as the canvas needs it: the display list, the box each prim paints into, and enough
/// of the cell tree to select groups and to know what can be moved and resized.
#[derive(Debug, Default)]
pub struct Sheet {
    /// The page it was made from, which a drag's preview edits a copy of.
    pub page: Page,
    pub scene: Scene,
    /// Where and when the page is shown, which labels with placeholders fill in.
    pub ctx: Context,
    /// Per prim, in page units: what culling tests against the viewport.
    pub bounds: Vec<Rect>,
    /// The page and the drawing together, which is what the canvas scrolls over.
    pub extent: Rect,
    /// The grid a drag snaps to, `None` where the page has it off.
    pub grid: Option<f64>,
    /// Whether a move shows guides.
    pub guides: bool,
    /// Each cell's parent, up to but not including its layer.
    parents: HashMap<CellId, CellId>,
    /// The rectangle of every vertex, absolute and unrotated: what resizing works on.
    rects: HashMap<CellId, Rect>,
    edges: HashSet<CellId>,
    /// Cells draw.io would not let be moved (`movable=0`, `locked=1`).
    pinned: HashSet<CellId>,
    /// Each turned vertex's `rotation`, in degrees.
    rotations: HashMap<CellId, f64>,
    /// Each unlocked, connectable vertex's connection points, with the constraint that pins an
    /// edge end to each.
    anchors: HashMap<CellId, Vec<(Point, Constraint)>>,
    /// Vertices whose style says they are not to be turned (`rotatable=0`).
    unturnable: HashSet<CellId>,
}

impl Sheet {
    pub fn of(page: &Page, ctx: &Context) -> Sheet {
        let scene = accent_drawio::scene_with(page, ctx);
        let bounds: Vec<Rect> = scene.prims.iter().map(|p| p.bounds()).collect();
        let (w, h) = page.size();
        let extent = bounds
            .iter()
            .fold(Rect::new(0.0, 0.0, w, h), |acc, b| acc.union(b));
        let layers: HashSet<&str> = page.layers().iter().map(|c| c.id.as_str()).collect();
        // A cell on a locked layer, or under a locked group, paints locked: pinned like one
        // locked itself.
        let locked: HashSet<CellId> = scene
            .prims
            .iter()
            .filter(|p| p.locked())
            .map(|p| p.cell().to_string())
            .collect();
        let mut sheet = Sheet {
            page: page.clone(),
            scene,
            ctx: *ctx,
            bounds,
            extent,
            grid: page.grid(),
            guides: page.guides(),
            ..Sheet::default()
        };
        for cell in &page.cells {
            let Some(parent) = cell.parent.as_deref() else {
                continue;
            };
            if layers.contains(cell.id.as_str()) {
                continue;
            }
            if !layers.contains(parent) {
                sheet.parents.insert(cell.id.clone(), parent.to_string());
            }
            if cell.edge {
                sheet.edges.insert(cell.id.clone());
            }
            if let Some(r) = page.absolute_rect(&cell.id) {
                sheet.rects.insert(cell.id.clone(), r);
            }
            let turned = cell
                .style
                .get("rotation")
                .and_then(|r| r.trim().parse::<f64>().ok());
            if let Some(deg) = turned.filter(|d| *d != 0.0 && d.is_finite()) {
                sheet.rotations.insert(cell.id.clone(), deg);
            }
            if cell.style.get("movable") == Some("0")
                || cell.style.get("locked") == Some("1")
                || locked.contains(&cell.id)
            {
                sheet.pinned.insert(cell.id.clone());
            }
            if cell.style.get("rotatable") == Some("0") {
                sheet.unturnable.insert(cell.id.clone());
            }
            let connectable = !cell
                .attrs
                .iter()
                .any(|(k, v)| k == "connectable" && v == "0");
            if cell.vertex && connectable && !locked.contains(&cell.id) {
                let anchors = accent_drawio::anchors(page, &cell.id);
                if !anchors.is_empty() {
                    sheet.anchors.insert(cell.id.clone(), anchors);
                }
            }
        }
        sheet
    }

    pub fn parent(&self, id: &str) -> Option<&str> {
        self.parents.get(id).map(String::as_str)
    }

    /// Whether `id` is `ancestor` or somewhere under it.
    pub fn is_within(&self, id: &str, ancestor: &str) -> bool {
        let mut at = Some(id);
        while let Some(cur) = at {
            if cur == ancestor {
                return true;
            }
            at = self.parent(cur);
        }
        false
    }

    pub fn is_edge(&self, id: &str) -> bool {
        self.edges.contains(id)
    }

    pub fn is_pinned(&self, id: &str) -> bool {
        self.pinned.contains(id)
    }

    /// Whether vertex `id` takes a rotate handle: one that can be moved and turned.
    pub fn is_turnable(&self, id: &str) -> bool {
        self.rects.contains_key(id) && !self.is_pinned(id) && !self.unturnable.contains(id)
    }

    /// How far a vertex is turned, in degrees.
    pub fn rotation(&self, id: &str) -> f64 {
        self.rotations.get(id).copied().unwrap_or(0.0)
    }

    /// A vertex's rectangle; `None` for an edge.
    pub fn rect(&self, id: &str) -> Option<Rect> {
        self.rects.get(id).copied()
    }

    /// The box a selected cell's frame is drawn around: its rectangle, or for an edge what it
    /// paints.
    pub fn frame_of(&self, id: &str) -> Option<Rect> {
        self.rect(id).or_else(|| self.scene.bounds_of(id))
    }

    /// The box a move aligns cell `id` by (`mxGraphHandler.getStateBounds`): its frame, a
    /// quarter-turned shape's extents swapped about its middle, as it shows.
    pub fn guide_box(&self, id: &str) -> Option<Rect> {
        let r = self.frame_of(id)?;
        let quarter = self.rects.contains_key(id) && self.rotation(id).rem_euclid(180.0) == 90.0;
        let c = r.centre();
        Some(match quarter {
            true => Rect::new(c.x - r.h / 2.0, c.y - r.w / 2.0, r.h, r.w),
            false => r,
        })
    }

    /// The boxes a move of `moving`, taken hold of at `pressed`, aligns to
    /// (`mxGraphHandler.getGuideStates` and the `isStateIgnored` draw.io gives it): the shapes
    /// shown on its layer that share its parent, the parent, and those an edge joins it to — or
    /// all of them when the parent holds fewer than two cells — none of them moving along.
    pub fn guide_boxes(&self, moving: &[CellId], pressed: &str) -> Vec<Rect> {
        let page = &self.page;
        let parent = page.cell(pressed).and_then(|c| c.parent.as_deref());
        let few = page
            .cells
            .iter()
            .filter(|c| c.parent.as_deref() == parent)
            .count()
            < 2;
        let joined: HashSet<&str> = page
            .cells
            .iter()
            .filter(|c| c.edge)
            .filter_map(|c| match (c.source.as_deref(), c.target.as_deref()) {
                (Some(s), t) if s == pressed => t,
                (s, Some(t)) if t == pressed => s,
                _ => None,
            })
            .collect();
        self.layer_shapes(pressed)
            .into_iter()
            .filter(|c| !moving.iter().any(|m| self.is_within(&c.id, m)))
            .filter(|c| {
                let id = Some(c.id.as_str());
                few || id == parent
                    || c.parent.as_deref() == parent
                    || joined.contains(c.id.as_str())
            })
            .filter_map(|c| self.guide_box(&c.id))
            .collect()
    }

    /// The shapes a resize of `id` takes a size from or lines a side up with
    /// (`mxVertexHandler.getSizeGuideStates`): the others shown on its layer and not inside it,
    /// within `area` — guides nobody can see snap nothing — the nearest first.
    pub fn size_guides(&self, id: &str, area: &Rect) -> Vec<Neighbour> {
        let Some(own) = self.guide_box(id) else {
            return Vec::new();
        };
        let away = |r: &Rect| {
            let (a, b) = (r.centre(), own.centre());
            (a.x - b.x).powi(2) + (a.y - b.y).powi(2)
        };
        let mut shapes: Vec<Neighbour> = self
            .layer_shapes(id)
            .into_iter()
            .filter(|c| !self.is_within(&c.id, id))
            .filter_map(|c| {
                let rect = self.guide_box(&c.id)?;
                let turn = self.rotation(&c.id);
                let turn = if turn.rem_euclid(180.0) == 90.0 {
                    0.0
                } else {
                    turn
                };
                Some(Neighbour { rect, turn })
            })
            .filter(|n| n.rect.w > 0.0 && n.rect.h > 0.0 && n.rect.intersects(area))
            .collect();
        shapes.sort_by(|a, b| away(&a.rect).total_cmp(&away(&b.rect)));
        shapes
    }

    /// The shapes shown on `id`'s layer, `id` among them: each vertex placed by a geometry of
    /// its own (not along an edge or on its parent) that is visible, and so is everything it is
    /// in. draw.io takes the guides from the layer new cells go into; this is `id`'s.
    fn layer_shapes(&self, id: &str) -> Vec<&accent_drawio::Cell> {
        let page = &self.page;
        let cells: HashMap<&str, &accent_drawio::Cell> =
            page.cells.iter().map(|c| (c.id.as_str(), c)).collect();
        // A cell's layer: the parent of its outermost ancestor below one.
        let layer_of = |id: &str| {
            let mut top = id;
            while let Some(p) = self.parent(top) {
                top = p;
            }
            cells.get(top).and_then(|c| c.parent.as_deref())
        };
        let shown = |id: &str| {
            let mut at = cells.get(id).copied();
            while let Some(cell) = at {
                if !cell.is_visible() {
                    return false;
                }
                at = cell.parent.as_deref().and_then(|p| cells.get(p).copied());
            }
            true
        };
        let layer = layer_of(id);
        page.cells
            .iter()
            .filter(|c| c.vertex && c.geometry.as_ref().is_some_and(|g| !g.relative))
            .filter(|c| shown(&c.id) && layer_of(&c.id) == layer)
            .collect()
    }

    /// What a press at `p` is on, given what is selected already: the cell a click there
    /// selects — the outermost group under the layer first, then one level further in on each
    /// click after that, draw.io's group-first selection — and the selected cell under the
    /// pointer, if there is one, which is what a drag from there moves.
    pub fn pick(&self, p: Point, tolerance: f64, selection: &[CellId]) -> Option<Pick> {
        let hit = self.scene.hit(p, tolerance)?;
        let mut chain = vec![hit];
        while let Some(parent) = self.parent(chain[chain.len() - 1]) {
            chain.push(parent);
        }
        let held = chain
            .iter()
            .position(|id| selection.iter().any(|s| s == id));
        let cell = match held {
            Some(0) => chain[0],
            Some(k) => chain[k - 1],
            None => chain[chain.len() - 1],
        };
        Some(Pick {
            cell: cell.to_string(),
            held: held.map(|k| chain[k].to_string()),
        })
    }

    /// The shape under `p` an edge can be attached to: the innermost vertex, never an edge.
    pub fn vertex_at(&self, p: Point, tolerance: f64) -> Option<CellId> {
        self.scene
            .hit(p, tolerance)
            .filter(|id| !self.is_edge(id) && self.rects.contains_key(*id))
            .map(str::to_string)
    }

    /// Whether `p` is inside vertex `id`, turned as it is drawn.
    pub fn contains(&self, id: &str, p: Point) -> bool {
        self.rect(id)
            .is_some_and(|r| r.contains(rotate(p, r.centre(), -self.rotation(id))))
    }

    /// The connection points of vertex `id`, none for anything else.
    pub fn anchors_of(&self, id: &str) -> &[(Point, Constraint)] {
        self.anchors.get(id).map_or(&[], Vec::as_slice)
    }

    /// The connection point nearest `p` within `reach`, on any shape that has them.
    pub fn anchor_near(&self, p: Point, reach: f64) -> Option<(CellId, Point, Constraint)> {
        self.anchors
            .iter()
            .flat_map(|(id, list)| list.iter().map(move |(at, c)| (id, *at, *c)))
            .map(|(id, at, c)| (at.distance(p), id, at, c))
            .filter(|(d, ..)| *d <= reach)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, id, at, c)| (id.clone(), at, c))
    }

    /// The handles between the ends of edge `id` and where they sit, the route being `route`
    /// (the page's, or a drag's preview's): none for an edge that cannot be bent (`bendable=0`,
    /// locked).
    pub fn knobs(&self, id: &str, route: &[Point]) -> Vec<(Knob, Point, bool)> {
        match self.edge_kind(id) {
            Some(kind) if !self.is_pinned(id) => {
                handle::knobs(kind, route, !self.waypoints(id).is_empty())
            }
            _ => Vec::new(),
        }
    }

    /// How edge `id`'s middle is handled; `None` for anything else or an edge that cannot be
    /// bent.
    pub fn edge_kind(&self, id: &str) -> Option<Kind> {
        let cell = self.page.cell(id).filter(|c| c.edge)?;
        let style = cell.style.resolve(true);
        let is_loop = cell.source.is_some() && cell.source == cell.target;
        let kind = handle::kind(&style, is_loop);
        // ponytail: a straight edge's bends and virtual bends come with the next change.
        let staged = !matches!(kind, Kind::Bends { .. });
        (style.get("bendable") != Some("0") && staged).then_some(kind)
    }

    /// Edge `id`'s waypoints, absolute.
    pub fn waypoints(&self, id: &str) -> Vec<Point> {
        let origin = self.page.origin_of(id);
        let points = self
            .page
            .cell(id)
            .and_then(|c| c.geometry.as_ref()?.points.clone());
        let at = |p: Point| Point::new(p.x + origin.x, p.y + origin.y);
        points.unwrap_or_default().into_iter().map(at).collect()
    }

    /// The shapes edge `id`'s source and target ends are on, and whether each is pinned there.
    pub fn terminals(&self, id: &str) -> [Option<Terminal>; 2] {
        let Some(cell) = self.page.cell(id) else {
            return [None, None];
        };
        let end = |on: &Option<CellId>, key: &str| {
            let rect = self.rect(on.as_deref()?)?;
            let pinned = cell.style.get(key).is_some();
            Some(Terminal { rect, pinned })
        };
        [end(&cell.source, "exitX"), end(&cell.target, "entryX")]
    }

    /// What an edge end dropped at `p` attaches to, its other end being at `other`: the
    /// connection point within `reach` that pins it, else the shape under it when `other` is
    /// outside that shape, else nothing (see [`Sheet::connect_ends`]).
    pub fn end_at(&self, p: Point, other: Point, tolerance: f64, reach: f64) -> End {
        match self.anchor_near(p, reach) {
            Some((id, at, c)) => (Some(id), at, Some(c)),
            None => {
                let under = self.vertex_at(p, tolerance);
                (under.filter(|v| !self.contains(v, other)), p, None)
            }
        }
    }

    /// What a connector drawn from `press` to `release` attaches to at either end. An end
    /// dropped within `reach` of a connection point is pinned there. Otherwise it takes the
    /// shape under it only when the other end is outside that shape: a line drawn within a
    /// shape — a big text box, a slide's background — is a line on it, and attaching it would
    /// start the arrow on the shape's far border and run it back through where it was drawn.
    pub fn connect_ends(
        &self,
        press: Point,
        release: Point,
        tolerance: f64,
        reach: f64,
    ) -> (End, End) {
        let source = self.end_at(press, release, tolerance, reach);
        let mut target = self.end_at(release, press, tolerance, reach);
        if target.0.is_some() && target.0 == source.0 {
            target = (None, release, None);
        }
        (source, target)
    }

    /// The cells a band drawn over the page takes: every whole top-level cell inside it.
    pub fn band(&self, band: Rect) -> Vec<CellId> {
        self.scene
            .cells_in(band)
            .into_iter()
            .filter(|id| self.parent(id).is_none())
            .collect()
    }

    /// Halfway along edge `id` as it is drawn: where its label goes.
    pub fn edge_middle(&self, id: &str) -> Option<Point> {
        self.scene.prims.iter().find_map(|p| match p {
            Prim::Path { cell, path, .. } if cell == id && self.is_edge(id) => midpoint(path),
            _ => None,
        })
    }

    /// Every unlocked cell in paint order that sits directly on a layer: what Select All
    /// takes, draw.io leaving what is on a locked layer out of a selection.
    pub fn top_level(&self) -> Vec<CellId> {
        let mut seen = HashSet::new();
        self.scene
            .prims
            .iter()
            .filter(|p| !p.locked())
            .map(|p| p.cell())
            .filter(|id| self.parent(id).is_none() && seen.insert(id.to_string()))
            .map(str::to_string)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use accent_drawio::geom::corners;

    fn near(a: Point, b: Point) -> bool {
        a.distance(b) < 1e-9
    }

    #[test]
    fn page_and_content_coordinates_round_trip() {
        for scale in [0.5, 2.0] {
            let (f, _) = frame(Rect::new(-50.0, 0.0, 500.0, 300.0), scale, (400.0, 400.0));
            let p = Point::new(12.5, -3.0);
            assert!(near(f.to_page(f.to_content(p)), p));
        }
    }

    #[test]
    fn a_small_drawing_is_centred_and_a_large_one_keeps_its_gap() {
        let (f, size) = frame(Rect::new(0.0, 0.0, 100.0, 100.0), 1.0, (500.0, 400.0));
        assert_eq!(size, (500.0, 400.0));
        assert_eq!((f.x, f.y), (200.0, 150.0));
        let (f, size) = frame(Rect::new(-10.0, 0.0, 1000.0, 100.0), 1.0, (500.0, 400.0));
        assert_eq!(size.0, 1000.0 + 2.0 * GAP);
        assert_eq!(f.to_content(Point::new(-10.0, 0.0)).x, GAP);
    }

    #[test]
    fn fitting_shows_the_whole_page_and_clamps() {
        let s = fit_scale((1000.0, 600.0), (500.0, 500.0));
        assert!((s - (500.0 - 2.0 * GAP) / 1000.0).abs() < 1e-9);
        assert_eq!(fit_scale((1.0, 1.0), (1e9, 1e9)), MAX_SCALE);
        assert_eq!(fit_scale((1e9, 1e9), (100.0, 100.0)), MIN_SCALE);
    }

    #[test]
    fn a_move_lands_the_origin_on_the_grid() {
        let d = snap_move(Point::new(13.0, 20.0), Point::new(5.0, 4.0), 10.0);
        assert_eq!((d.x, d.y), (7.0, 0.0));
        assert_eq!(snap(14.9, 10.0), 10.0);
    }

    #[test]
    fn handles_are_found_by_their_corner_or_edge_and_nothing_else() {
        let r = Rect::new(0.0, 0.0, 100.0, 50.0);
        assert_eq!(
            handle_at(&r, Point::new(2.0, 1.0), 6.0),
            Some(Handle::NorthWest)
        );
        assert_eq!(
            handle_at(&r, Point::new(50.0, 52.0), 6.0),
            Some(Handle::South)
        );
        assert_eq!(handle_at(&r, Point::new(50.0, 25.0), 6.0), None);
    }

    #[test]
    fn a_turned_shape_resizes_in_its_own_frame_keeping_its_far_side() {
        // A quarter turn: the east handle sits at the bottom on the page.
        let r = Rect::new(0.0, 0.0, 100.0, 20.0);
        let before = corners(&r, r.centre(), 90.0);
        let after = resize_rotated(
            &r,
            90.0,
            Handle::East,
            Point::new(0.0, 10.0),
            Some(10.0),
            |_, _| {},
        );
        assert!(
            (after.w - 110.0).abs() < 1e-9 && (after.h - 20.0).abs() < 1e-9,
            "{after:?}"
        );
        let moved = corners(&after, after.centre(), 90.0);
        // The west side (the top edge on the page) is where it was.
        for (a, b) in [(before[0], moved[0]), (before[3], moved[3])] {
            assert!(a.distance(b) < 1e-9, "{a:?} vs {b:?}");
        }
        assert_eq!(
            resize_rotated(
                &r,
                0.0,
                Handle::East,
                Point::new(4.0, 0.0),
                Some(10.0),
                |_, _| {}
            )
            .w,
            100.0
        );
    }

    #[test]
    fn a_connector_attaches_only_to_a_shape_its_other_end_is_outside() {
        let mut page = Page::blank("P", "p");
        let cell =
            |id: &str, r: Rect, style: &str| accent_drawio::Cell::new_vertex(id, "1", r, style, "");
        page.cells
            .push(cell("box", Rect::new(0.0, 0.0, 400.0, 300.0), "text;"));
        page.cells
            .push(cell("s", Rect::new(50.0, 50.0, 40.0, 20.0), ""));
        page.cells
            .push(cell("t", Rect::new(250.0, 200.0, 40.0, 20.0), ""));
        let sheet = Sheet::of(&page, &Context::default());
        let ends = |a: (f64, f64), b: (f64, f64)| {
            let (s, t) = sheet.connect_ends(Point::new(a.0, a.1), Point::new(b.0, b.1), 1.0, 1.0);
            (s.0, t.0)
        };
        let some = |id: &str| Some(id.to_string());
        // Drawn within the text box: on it, attached to nothing.
        assert_eq!(ends((150.0, 150.0), (350.0, 250.0)), (None, None));
        // From a shape out into the box: attached at the shape only.
        assert_eq!(ends((70.0, 60.0), (350.0, 100.0)), (some("s"), None));
        // Shape to shape, both inside the box: both attached.
        assert_eq!(ends((70.0, 60.0), (270.0, 210.0)), (some("s"), some("t")));
        // From outside everything onto a shape.
        assert_eq!(ends((500.0, 500.0), (270.0, 210.0)), (None, some("t")));
    }

    #[test]
    fn a_move_aligns_to_its_siblings_and_the_shapes_joined_to_it() {
        let xml = r#"<mxfile><diagram name="P" id="p"><mxGraphModel><root>
            <mxCell id="0"/><mxCell id="1" parent="0"/>
            <mxCell id="a" parent="1" vertex="1" style=""><mxGeometry x="0" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="b" parent="1" vertex="1" style="rotation=90;"><mxGeometry x="20" y="0" width="10" height="30" as="geometry"/></mxCell>
            <mxCell id="g" parent="1" vertex="1" style="group"><mxGeometry x="100" y="0" width="50" height="50" as="geometry"/></mxCell>
            <mxCell id="c" parent="g" vertex="1" style=""><mxGeometry x="0" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="d" parent="g" vertex="1" style=""><mxGeometry x="20" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="e" parent="1" edge="1" source="a" target="c"><mxGeometry relative="1" as="geometry"/></mxCell>
            </root></mxGraphModel></diagram></mxfile>"#;
        let file = accent_drawio::File::from_bytes(xml.as_bytes()).unwrap();
        let sheet = Sheet::of(&file.pages()[0], &Context::default());
        let ids = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        // A quarter-turned shape aligns by its extents as they show.
        let turned = Rect::new(10.0, 10.0, 30.0, 10.0);
        assert_eq!(sheet.guide_box("b"), Some(turned));
        // Moving a: its siblings b and g, and c at the edge's other end, not d inside g.
        let group = Rect::new(100.0, 0.0, 50.0, 50.0);
        let c = Rect::new(100.0, 0.0, 10.0, 10.0);
        assert_eq!(sheet.guide_boxes(&ids(&["a"]), "a"), [turned, group, c]);
        // Moving c inside g: a at the edge's other end, the group and its sibling d, not b.
        let (a, d) = (
            Rect::new(0.0, 0.0, 10.0, 10.0),
            Rect::new(120.0, 0.0, 10.0, 10.0),
        );
        assert_eq!(sheet.guide_boxes(&ids(&["c"]), "c"), [a, group, d]);
    }

    #[test]
    fn a_locked_layer_pins_its_cells_and_select_all_leaves_them_out() {
        let xml = r#"<mxfile><diagram name="P" id="p"><mxGraphModel><root>
            <mxCell id="0"/><mxCell id="L" parent="0" style="locked=1;"/><mxCell id="C" parent="0"/>
            <mxCell id="a" parent="L" vertex="1" style=""><mxGeometry x="0" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="b" parent="C" vertex="1" style=""><mxGeometry x="20" y="0" width="10" height="10" as="geometry"/></mxCell>
            </root></mxGraphModel></diagram></mxfile>"#;
        let file = accent_drawio::File::from_bytes(xml.as_bytes()).unwrap();
        let sheet = Sheet::of(&file.pages()[0], &Context::default());
        assert!(sheet.is_pinned("a") && !sheet.is_pinned("b"));
        assert_eq!(sheet.top_level(), vec!["b".to_string()]);
    }

    #[test]
    fn an_edge_s_middle_is_halfway_along_its_bends() {
        let path = [
            PathCmd::MoveTo(Point::new(0.0, 0.0)),
            PathCmd::LineTo(Point::new(100.0, 0.0)),
            PathCmd::LineTo(Point::new(100.0, 50.0)),
        ];
        assert!(near(midpoint(&path).unwrap(), Point::new(75.0, 0.0)));
        assert_eq!(midpoint(&[]), None);
    }

    #[test]
    fn a_connector_end_snaps_to_the_nearest_connection_point() {
        let mut page = Page::blank("P", "p");
        let cell = |id: &str, r: Rect| accent_drawio::Cell::new_vertex(id, "1", r, "", "");
        page.cells
            .push(cell("s", Rect::new(50.0, 50.0, 40.0, 20.0)));
        page.cells
            .push(cell("t", Rect::new(250.0, 200.0, 40.0, 20.0)));
        let sheet = Sheet::of(&page, &Context::default());
        let (s, t) = sheet.connect_ends(Point::new(91.0, 61.0), Point::new(249.0, 209.0), 1.0, 3.0);
        assert_eq!((s.0.as_deref(), s.1), (Some("s"), Point::new(90.0, 60.0)));
        assert_eq!(s.2.map(|c| c.point), Some(Point::new(1.0, 0.5)));
        assert_eq!((t.0.as_deref(), t.1), (Some("t"), Point::new(250.0, 210.0)));
        assert_eq!(t.2.map(|c| c.point), Some(Point::new(0.0, 0.5)));
        // Away from every point, an end is where it was dropped and pins nothing.
        let (_, free) =
            sheet.connect_ends(Point::new(91.0, 61.0), Point::new(400.0, 400.0), 1.0, 3.0);
        assert_eq!(free, (None, Point::new(400.0, 400.0), None));
    }

    #[test]
    fn a_rotation_follows_the_pointer_s_bearing_and_snaps_near_the_handle() {
        let c = Point::new(0.0, 0.0);
        // A handle straight up, dragged a quarter round on its own circle: 90°.
        let up = Point::new(0.0, -50.0);
        assert_eq!(rotation_to(c, up, Point::new(50.0, 0.0), false), 90.0);
        // Near the circle, 15° steps; well outside it, whole degrees; free, tenths.
        let near = Point::new(50.0, -8.0);
        assert_eq!(rotation_to(c, up, near, false), 75.0);
        let far = Point::new(200.0, -32.0);
        assert_eq!(rotation_to(c, up, far, false), 81.0);
        assert_eq!(rotation_to(c, up, far, true), 80.9);
        // A handle to the upper right held straight down is turned past a half: -135°, not 225°.
        let corner = Point::new(50.0, -50.0);
        assert_eq!(
            rotation_to(c, corner, Point::new(-50.0, 50.0), false),
            180.0
        );
        assert_eq!(
            rotation_to(c, corner, Point::new(-50.0, 0.0), false),
            -135.0
        );
    }

    #[test]
    fn a_turned_handle_shows_the_cursor_of_the_way_it_points() {
        assert_eq!(Handle::East.turned(90.0), Handle::South);
        assert_eq!(Handle::North.turned(-45.0), Handle::NorthWest);
        assert_eq!(Handle::NorthWest.turned(30.0), Handle::North);
    }

    #[test]
    fn a_turned_shape_resizes_to_a_size_on_the_grid() {
        let r = Rect::new(3.0, 7.0, 100.0, 20.0);
        let after = resize_rotated(
            &r,
            90.0,
            Handle::East,
            Point::new(0.0, 14.0),
            Some(10.0),
            |_, _| {},
        );
        assert!(
            (after.w - 110.0).abs() < 1e-9 && (after.h - 20.0).abs() < 1e-9,
            "{after:?}"
        );
        let free = resize_rotated(
            &r,
            90.0,
            Handle::East,
            Point::new(0.0, 14.0),
            None,
            |_, _| {},
        );
        assert!((free.w - 114.0).abs() < 1e-9, "{free:?}");
    }

    #[test]
    fn resizing_moves_only_the_held_edges_and_keeps_a_minimum() {
        let r = Rect::new(10.0, 10.0, 100.0, 50.0);
        let e = resize_by(&r, Handle::East, Point::new(33.0, 99.0), None);
        assert_eq!(e, Rect::new(10.0, 10.0, 133.0, 50.0));
        let nw = resize_by(&r, Handle::NorthWest, Point::new(500.0, 500.0), None);
        assert_eq!((nw.w, nw.h), (MIN_SIZE, MIN_SIZE));
        assert_eq!((nw.right(), nw.bottom()), (r.right(), r.bottom()));
        let snapped = resize_by(&r, Handle::SouthEast, Point::new(4.0, 4.0), Some(10.0));
        assert_eq!((snapped.right(), snapped.bottom()), (110.0, 60.0));
    }
}
