//! The canvas's arithmetic, free of GTK so it is tested without a display: where a page point
//! lands in the scrolled content, what fitting means, snapping to the grid, the selection's
//! handles, and which cell a click selects.

use std::collections::{HashMap, HashSet};

use accent_drawio::geom::rotate;
use accent_drawio::{CellId, Page, Point, Rect, Scene};

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
/// does not hold stays where it was on the page — draw.io's rule. No grid for a turned shape,
/// whose edges do not run along it.
pub fn resize_rotated(
    r: &Rect,
    rotation: f64,
    handle: Handle,
    delta: Point,
    grid: Option<f64>,
) -> Rect {
    if rotation == 0.0 {
        return resize_by(r, handle, delta, grid);
    }
    let origin = Point::default();
    let local = resize_by(r, handle, rotate(delta, origin, -rotation), None);
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

/// The four corners of `r` turned `rotation` degrees about its centre, clockwise from the top
/// left: a turned shape's outline.
pub fn corners(r: &Rect, rotation: f64) -> [Point; 4] {
    let c = r.centre();
    [
        Point::new(r.x, r.y),
        Point::new(r.right(), r.y),
        Point::new(r.right(), r.bottom()),
        Point::new(r.x, r.bottom()),
    ]
    .map(|p| rotate(p, c, rotation))
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
    pub scene: Scene,
    /// Per prim, in page units: what culling tests against the viewport.
    pub bounds: Vec<Rect>,
    /// The page and the drawing together, which is what the canvas scrolls over.
    pub extent: Rect,
    pub grid: f64,
    /// Each cell's parent, up to but not including its layer.
    parents: HashMap<CellId, CellId>,
    /// The rectangle of every vertex, absolute and unrotated: what resizing works on.
    rects: HashMap<CellId, Rect>,
    edges: HashSet<CellId>,
    /// Cells draw.io would not let be moved (`movable=0`, `locked=1`).
    pinned: HashSet<CellId>,
    /// Each turned vertex's `rotation`, in degrees.
    rotations: HashMap<CellId, f64>,
}

impl Sheet {
    pub fn of(page: &Page) -> Sheet {
        let scene = accent_drawio::scene(page);
        let bounds: Vec<Rect> = scene.prims.iter().map(|p| p.bounds()).collect();
        let (w, h) = page.size();
        let extent = bounds
            .iter()
            .fold(Rect::new(0.0, 0.0, w, h), |acc, b| acc.union(b));
        let layers: HashSet<&str> = page.layers().iter().map(|c| c.id.as_str()).collect();
        let mut sheet = Sheet {
            scene,
            bounds,
            extent,
            grid: page.grid_size(),
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
            if cell.style.get("movable") == Some("0") || cell.style.get("locked") == Some("1") {
                sheet.pinned.insert(cell.id.clone());
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

    /// What a connector drawn from `press` to `release` attaches to at either end. An end takes
    /// the shape under it only when the other end is outside that shape: a line drawn within a
    /// shape — a big text box, a slide's background — is a line on it, and attaching it would
    /// start the arrow on the shape's far border and run it back through where it was drawn.
    pub fn connect_ends(
        &self,
        press: Point,
        release: Point,
        tolerance: f64,
    ) -> (Option<CellId>, Option<CellId>) {
        let source = self
            .vertex_at(press, tolerance)
            .filter(|s| !self.contains(s, release));
        let target = self
            .vertex_at(release, tolerance)
            .filter(|t| !self.contains(t, press) && source.as_ref() != Some(t));
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

    /// Every cell in paint order that sits directly on a layer: what Select All takes.
    pub fn top_level(&self) -> Vec<CellId> {
        let mut seen = HashSet::new();
        self.scene
            .prims
            .iter()
            .map(|p| p.cell())
            .filter(|id| self.parent(id).is_none() && seen.insert(id.to_string()))
            .map(str::to_string)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let before = corners(&r, 90.0);
        let after = resize_rotated(&r, 90.0, Handle::East, Point::new(0.0, 10.0), Some(10.0));
        assert!(
            (after.w - 110.0).abs() < 1e-9 && (after.h - 20.0).abs() < 1e-9,
            "{after:?}"
        );
        let moved = corners(&after, 90.0);
        // The west side (the top edge on the page) is where it was.
        for (a, b) in [(before[0], moved[0]), (before[3], moved[3])] {
            assert!(a.distance(b) < 1e-9, "{a:?} vs {b:?}");
        }
        assert_eq!(
            resize_rotated(&r, 0.0, Handle::East, Point::new(4.0, 0.0), Some(10.0)).w,
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
        let sheet = Sheet::of(&page);
        let ends = |a: (f64, f64), b: (f64, f64)| {
            let (s, t) = sheet.connect_ends(Point::new(a.0, a.1), Point::new(b.0, b.1), 1.0);
            (
                s.as_deref().map(str::to_string),
                t.as_deref().map(str::to_string),
            )
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
