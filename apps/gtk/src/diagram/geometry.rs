//! Display-independent canvas geometry for headless tests: coordinate transforms,
//! fit, grid snapping, selection handles and hit testing.

use std::collections::{HashMap, HashSet};

use accent_drawio::geom::{bounding_box, rotate};
use accent_drawio::guide::Neighbour;
use accent_drawio::handle::{self, Kind, Knob, Terminal};
use accent_drawio::{CellId, Constraint, Context, Page, PathCmd, Point, Prim, Rect, Scene};

use crate::zoom::clamp_scale;

/// Room left around the drawing, in pixels on screen.
pub const GAP: f64 = 24.0;
/// A selection handle's side, in pixels on screen — the PDF's Select box's.
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

/// How big the page is drawn: fitted to the window, by its width or its height alone as a PDF's
/// Fit Width and Fit Height fit it, or at a chosen scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Zoom {
    Fit,
    Width,
    Height,
    Scale(f64),
}

/// Where page coordinates land in the scrolled content: `content = page · scale + (x, y)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub scale: f64,
    pub x: f64,
    pub y: f64,
    /// The pixels a page unit takes where the content goes out, which a one-pixel line is kept
    /// whole on: the scale on screen, an export's own where it paints its content in more detail.
    pub pixels: f64,
}

impl Default for Frame {
    fn default() -> Frame {
        Frame {
            scale: 1.0,
            x: 0.0,
            y: 0.0,
            pixels: 1.0,
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
    // On whole pixels, which keep a one-pixel line on them as draw.io's do.
    let frame = Frame {
        scale,
        x: ((size.0 - dw) / 2.0 - extent.x * scale).round(),
        y: ((size.1 - dh) / 2.0 - extent.y * scale).round(),
        pixels: scale,
    };
    (frame, size)
}

/// The scale `zoom` shows the page at in `viewport`, [`GAP`] around what it fits.
pub fn zoom_scale(zoom: Zoom, page: (f64, f64), viewport: (f64, f64)) -> f64 {
    let width = (viewport.0 - 2.0 * GAP) / page.0.max(1.0);
    let height = (viewport.1 - 2.0 * GAP) / page.1.max(1.0);
    clamp_scale(match zoom {
        Zoom::Fit => width.min(height),
        Zoom::Width => width,
        Zoom::Height => height,
        Zoom::Scale(s) => s,
    })
}

/// How far a touchpad swipe pushes on past the edge of a presented page to turn it: about what a
/// wheel notch scrolls, `GtkScrolledWindow` scrolling 2.5 px for each of a swipe's.
pub const SWIPE_TURN: f64 = 40.0;

/// How many times a presented page's push turns a page being edited: three notches, or a swipe of
/// 120 px, the page pulled off its edge meanwhile ([`Overshoot::pull`]).
const SPRING: f64 = 3.0;

/// How far the page is pulled off its edge as the push nears a turn, in pixels: 21 a notch.
pub const PULL_PX: f64 = 64.0;

/// How long a push past the edge rests before the page springs back, in ms.
pub const REST_MS: u64 = 300;

/// How long a push past the edge is remembered after its last scroll, in ms, so slow notches add
/// up to a turn though the page springs back between them.
const FORGET_MS: u32 = 1000;

/// Turn pages only when scrolling starts at an edge, preventing a scroll that
/// reaches the edge from also turning it. A wheel turns once per notch; a touchpad
/// swipe turns only if it has not scrolled or turned yet, preventing multi-page flings.
/// While editing a push turns only at [`SPRING`] times that, so a steady spin turns a page every
/// third notch.
#[derive(Debug, Default, Clone, Copy)]
pub struct Overshoot {
    /// How far the push has gone past the edge, down positive.
    past: f64,
    /// The swipe under way has scrolled or turned the page, and turns nothing more.
    spent: bool,
    /// When the push last went on, the event's time in ms.
    at: u32,
}

impl Overshoot {
    /// One plain scroll of `dy` (down positive: notches, or a swipe's pixels) at event time `time`
    /// in ms, `room` saying whether the page can still scroll that way, and `spring` whether it is
    /// being edited. `Some(forward)` turns the page.
    pub fn scroll(
        &mut self,
        dy: f64,
        room: bool,
        swipe: bool,
        spring: bool,
        time: u32,
    ) -> Option<bool> {
        if room {
            self.past = 0.0;
            self.spent = swipe;
            return None;
        }
        if self.spent {
            return None;
        }
        // A push back the other way, or one let go for [`FORGET_MS`], starts again.
        if self.past * dy < 0.0 || time.wrapping_sub(self.at) > FORGET_MS {
            self.past = 0.0;
        }
        self.at = time;
        self.past += dy;
        if self.past.abs() < reach(swipe, spring) {
            return None;
        }
        *self = Overshoot {
            past: 0.0,
            spent: swipe,
            at: time,
        };
        Some(dy > 0.0)
    }

    /// How far a page being edited is pulled off its edge, in pixels, down positive.
    pub fn pull(&self, swipe: bool) -> f64 {
        self.past / reach(swipe, true) * PULL_PX
    }
}

/// How far a push goes past the edge to turn the page: a notch or [`SWIPE_TURN`], [`SPRING`]
/// times that while editing.
fn reach(swipe: bool, spring: bool) -> f64 {
    let push = if swipe { SWIPE_TURN } else { 1.0 };
    if spring { push * SPRING } else { push }
}

pub fn snap(v: f64, grid: f64) -> f64 {
    (v / grid).round() * grid
}

/// `p` on the grid, where there is one.
pub fn snap_point(p: Point, grid: Option<f64>) -> Point {
    grid.map_or(p, |g| Point::new(snap(p.x, g), snap(p.y, g)))
}

/// The steps a line's angle and a turn keep to with Shift held, in degrees: draw.io's raster
/// near the rotate handle, the axes and the diagonals among them.
pub const ANGLE_STEP: f64 = 15.0;

/// The axes a move of `delta` may take: both, or with `lock` (Shift, draw.io's
/// `isConstrainedEvent`) only the one the pointer has gone furthest along.
pub fn move_axes(delta: Point, lock: bool) -> [bool; 2] {
    let across = delta.x.abs() >= delta.y.abs();
    [!lock || across, !lock || !across]
}

/// Where a line's end at `p` goes with Shift held, its other end at `anchor`: turned onto the
/// nearest step of [`ANGLE_STEP`], its length on the grid where there is one. To a hundredth,
/// which keeps float dust out of the file.
pub fn snap_angle(anchor: Point, p: Point, grid: Option<f64>) -> Point {
    let (dx, dy) = (p.x - anchor.x, p.y - anchor.y);
    let angle = (dy.atan2(dx).to_degrees() / ANGLE_STEP).round() * ANGLE_STEP;
    let length = grid.map_or(dx.hypot(dy), |g| snap(dx.hypot(dy), g));
    let (sin, cos) = angle.to_radians().sin_cos();
    let hundredth = |v: f64| (v * 100.0).round() / 100.0;
    Point::new(
        hundredth(anchor.x + length * cos),
        hundredth(anchor.y + length * sin),
    )
}

/// The grid a drag snaps to and the canvas draws at `scale` pixels a unit: the page's `size`
/// halved while the half is still 8 px on screen, so snapping gets finer as the page is zoomed
/// in, and doubled while it is under 4 px, as draw.io spaces its lines (`minGridSize`).
pub fn grid_step(size: f64, scale: f64) -> f64 {
    const FINE_PX: f64 = 8.0;
    const COARSE_PX: f64 = 4.0;
    let mut step = size;
    while step / 2.0 * scale >= FINE_PX {
        step /= 2.0;
    }
    while step * scale < COARSE_PX {
        step *= 2.0;
    }
    step
}

/// A move of `delta` corrected so that `origin` lands on the grid, as draw.io snaps a drag.
pub fn snap_move(origin: Point, delta: Point, grid: f64) -> Point {
    Point::new(
        snap(origin.x + delta.x, grid) - origin.x,
        snap(origin.y + delta.y, grid) - origin.y,
    )
}

/// Resize in the rotated shape's local frame, keeping the undragged side fixed on
/// the page. Snap size rather than edges for rotated shapes (`mxVertexHandler.union`).
/// `snap` receives the local resized box and original `r` before placement for size guides.
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

/// Rotation from the pointer bearing relative to the initial handle, in content
/// coordinates (`mxVertexHandler.rotateVertex`). Snap to 15°, 5° or 1° with increasing
/// distance from the handle circle, or 0.1° with `free`, and to 15° wherever the pointer is
/// with `constrained` (Shift). Normalize to (-180, 180].
pub fn rotation_to(
    centre: Point,
    handle: Point,
    pointer: Point,
    free: bool,
    constrained: bool,
) -> f64 {
    let alpha = bearing(centre, pointer) - bearing(centre, handle);
    let raster = match pointer.distance(centre) - handle.distance(centre) {
        _ if constrained => ANGLE_STEP,
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

/// `out`, resized from `r` by `handle`, kept to `r`'s proportions as draw.io keeps a shape with
/// `aspect=fixed`: a top or bottom handle, and the right-hand corners, set the height and the
/// width follows it, any other handle the width; only the top-left corner grows up and left.
// mxVertexHandler.union, mxVertexHandler.js 1772-1795
pub fn keep_proportions(out: Rect, r: &Rect, handle: Handle) -> Rect {
    if r.w <= 0.0 || r.h <= 0.0 {
        return out;
    }
    let aspect = r.w / r.h;
    let (w, h) = match handle {
        Handle::North | Handle::NorthEast | Handle::South | Handle::SouthEast => {
            (out.h * aspect, out.h)
        }
        _ => (out.w, out.w / aspect),
    };
    match handle {
        Handle::NorthWest => Rect::new(out.right() - w, out.bottom() - h, w, h),
        _ => Rect::new(out.x, out.y, w, h),
    }
}

/// One end of a connector being drawn: the shape it attaches to, if any, where it is, and the
/// connection point that pins it when it was dropped on one.
pub type End = (Option<CellId>, Point, Option<Constraint>);

/// What a press landed on: see [`Sheet::pick`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    pub cell: CellId,
    pub held: Option<CellId>,
    /// The group with `transparentBounds=1` that `cell`, the one cell selected, is in, which a
    /// click on it goes out to.
    pub out: Option<CellId>,
}

impl Pick {
    /// What a click on a selected cell selects once released where it was pressed, draw.io's
    /// delayed selection (`mxGraphHandler.selectDelayed`): the cell under the pointer alone,
    /// one level into a selected group or one of several cells selected; `None` where that is the
    /// whole selection already. draw.io's climbs from a selected shape back out to its group
    /// instead, which accent leaves out (user decision 2026-11-15) but for a group with
    /// `transparentBounds=1`, which only such a click selects (`Graph.selectCellForEvent`).
    pub fn clicked(&self, selection: &[CellId]) -> Option<Vec<CellId>> {
        if let Some(group) = &self.out {
            return Some(vec![group.clone()]);
        }
        (selection != std::slice::from_ref(&self.cell)).then(|| vec![self.cell.clone()])
    }
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
    /// The sheet the page is drawn on, `None` with its page view off (`page="0"`), where the
    /// drawing is on the canvas alone, as draw.io shows it.
    pub page_rect: Option<Rect>,
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
    /// Each vertex's line width, which its selection frame keeps clear of.
    lines: HashMap<CellId, f64>,
    /// Groups with `transparentBounds=1`, whose rectangle is what they hold and whose geometry
    /// stays at 0,0,0,0 as draw.io keeps it: framed and moved, never resized or turned.
    derived: HashSet<CellId>,
}

impl Sheet {
    pub fn of(page: &Page, ctx: &Context) -> Sheet {
        let scene = accent_drawio::scene_with(page, ctx);
        let bounds: Vec<Rect> = scene.prims.iter().map(|p| p.bounds()).collect();
        let (w, h) = page.size();
        let page_rect = page.page_view().then(|| Rect::new(0.0, 0.0, w, h));
        // A blank page without a sheet is the size its sheet would be.
        let extent = bounds
            .iter()
            .copied()
            .chain(page_rect)
            .reduce(|a, b| a.union(&b))
            .unwrap_or(Rect::new(0.0, 0.0, w, h));
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
            page_rect,
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
            if let Some(r) = accent_drawio::edit::bounds(page, cell) {
                sheet.rects.insert(cell.id.clone(), r);
            }
            if cell.vertex && cell.style.get("transparentBounds") == Some("1") {
                sheet.derived.insert(cell.id.clone());
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
            if cell.vertex {
                let style = cell.style.resolve(false);
                let line = style
                    .color("strokeColor")
                    .map(|_| style.num("strokeWidth", 1.0));
                sheet
                    .lines
                    .insert(cell.id.clone(), line.unwrap_or(0.0).max(0.0));
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

    /// What fitting shows and opening centres: the sheet, or the drawing where there is none.
    pub fn shown(&self) -> Rect {
        self.page_rect.unwrap_or(self.extent)
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

    /// Whether cell `id` takes a rotate handle: a vertex, or a line with neither end on a shape,
    /// whose points are all its own, that can be moved and turned.
    pub fn is_turnable(&self, id: &str) -> bool {
        let line = || {
            (self.page.cell(id)).is_some_and(|c| c.edge && c.source.is_none() && c.target.is_none())
        };
        (self.rects.contains_key(id) || line())
            && !self.is_pinned(id)
            && !self.unturnable.contains(id)
            && !self.derived.contains(id)
    }

    /// Whether vertex `id` takes resize handles: a shape that can be moved, and not a group whose
    /// size is what it holds.
    pub fn is_resizable(&self, id: &str) -> bool {
        self.rects.contains_key(id) && !self.is_pinned(id) && !self.derived.contains(id)
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

    /// How far outside [`Sheet::frame_of`] cell `id`'s selection frame sits, in page units, as
    /// the PDF Select tool's sits outside the ink: half a shape's line and a unit, or a unit
    /// beyond an edge's painted box, which holds its line already.
    pub fn padding(&self, id: &str) -> f64 {
        self.lines.get(id).copied().unwrap_or(0.0) / 2.0 + 1.0
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

    /// Stationary move-guide boxes on the pressed cell's layer: siblings, parent and
    /// connected shapes, or all shapes when the parent has fewer than two children
    /// (`mxGraphHandler.getGuideStates`, draw.io's `isStateIgnored`).
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

    /// Visible resize-guide candidates on `id`'s layer, excluding descendants and shapes
    /// outside `area`, nearest first (`mxVertexHandler.getSizeGuideStates`). Offscreen
    /// guides cannot snap.
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

    /// Visible, non-relative vertices on `id`'s layer, including `id`, with visible
    /// ancestors. Unlike draw.io's insertion-layer guides, these use the moved cell's layer.
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

    /// Resolve click selection and drag target at `p`. Clicks enter groups one level
    /// at a time, starting at the outermost group below a layer, a container's shape at once and
    /// one level below a group that holds the selection; drags move the already selected cell
    /// under the pointer, matching draw.io.
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
            None => *chain
                .iter()
                .find(|id| !self.climbs(id, selection))
                .unwrap_or(&chain[chain.len() - 1]),
        };
        // The one cell selected, clicked again, goes out to the group with
        // `transparentBounds=1` it is in, as draw.io's does (`Graph.selectCellForEvent`).
        let again = held == Some(0) && selection.len() == 1;
        let out = self
            .parent(cell)
            .filter(|p| again && self.derived.contains(*p));
        Some(Pick {
            cell: cell.to_string(),
            held: held.map(|k| chain[k].to_string()),
            out: out.map(str::to_string),
        })
    }

    /// Whether a click on `id`, with nothing above it selected, selects its group instead, as
    /// draw.io's first click does (`mxGraphHandler.isPropagateSelectionCell`, as `Graph.js`
    /// and `Editor.js` change it): always out of a parent with `selectParentFirst=1` and never
    /// out of one with `transparentBounds=1` or out of a connector, else not while the parent
    /// holds the selection, a sibling's (`isSiblingSelected`) or one further in, and not out of
    /// a container unless `id` is a label or a part of it. A table's rows and cells always go on
    /// to the table.
    fn climbs(&self, id: &str, selection: &[CellId]) -> bool {
        let Some(parent) = self.parent(id) else {
            return false;
        };
        if self.in_table(parent) {
            return true;
        }
        // A group that holds the selection keeps a click inside it, a level below it, where
        // draw.io's keeps one beside a selected sibling alone (user decision 2026-11-15).
        let opened = selection
            .iter()
            .any(|s| s != id && s != parent && self.is_within(s, parent));
        let says = |key: &str| {
            let cell = self.page.cell(parent);
            cell.is_some_and(|c| c.style.resolve(c.edge).get(key) == Some("1"))
        };
        if says("selectParentFirst") {
            return !opened;
        }
        if says("transparentBounds") || self.is_edge(parent) {
            return false;
        }
        let attached = self.page.cell(id).is_some_and(|c| {
            c.geometry.as_ref().is_some_and(|g| g.relative)
                || c.style.resolve(c.edge).get("part") == Some("1")
        });
        !opened && (attached || !self.is_container(parent))
    }

    /// Whether vertex `id` holds its shapes as a container, as draw.io's `Graph.isContainer`
    /// says: a swimlane or a table unless `container=0`, anything else with `container=1`.
    fn is_container(&self, id: &str) -> bool {
        let Some(cell) = self.page.cell(id).filter(|c| !c.edge) else {
            return false;
        };
        let style = cell.style.resolve(false);
        match style.get("shape") {
            Some("swimlane" | "table" | "tableRow") => style.get("container") != Some("0"),
            _ => style.get("container") == Some("1"),
        }
    }

    /// Whether `id` is a table or one of its rows, which hold their cells as the table lays them
    /// out.
    fn in_table(&self, id: &str) -> bool {
        let table = |id: &str| {
            let cell = self.page.cell(id);
            cell.is_some_and(|c| c.style.resolve(false).get("childLayout") == Some("tableLayout"))
        };
        table(id) || self.parent(id).is_some_and(table)
    }

    /// The group open while the selection is inside it: the innermost shape holding every cell
    /// selected, none of them being it, a table and its rows aside. Its outline shows and a paste
    /// goes into it (user decision 2026-11-15).
    pub fn open_group(&self, selection: &[CellId]) -> Option<CellId> {
        let (first, rest) = selection.split_first()?;
        let mut at = self.parent(first);
        while let Some(group) = at {
            let holds = rest.iter().all(|s| s != group && self.is_within(s, group));
            let shape = self.rects.contains_key(group) && !self.is_edge(group);
            if holds && shape && !self.in_table(group) {
                return Some(group.to_string());
            }
            at = self.parent(group);
        }
        None
    }

    /// What a click at `p` on no cell opens: the cells shown in the innermost selected group that
    /// `p` is inside, `None` where it is inside none.
    pub fn opened_at(&self, p: Point, selection: &[CellId]) -> Option<Vec<CellId>> {
        let depth = |id: &str| std::iter::successors(Some(id), |c| self.parent(c)).count();
        let drawn = |id: &str| {
            let mut prims = self.scene.prims.iter().filter(|p| !p.locked());
            prims.any(|p| self.is_within(p.cell(), id))
        };
        let shapes = |group: &str| -> Vec<CellId> {
            let inside = self.page.cells.iter();
            let inside = inside.filter(|c| self.parent(&c.id) == Some(group) && drawn(&c.id));
            inside.map(|c| c.id.clone()).collect()
        };
        selection
            .iter()
            .filter(|id| self.contains(id, p))
            .map(|id| (depth(id), shapes(id)))
            .filter(|(_, shapes)| !shapes.is_empty())
            .max_by_key(|(depth, _)| *depth)
            .map(|(_, shapes)| shapes)
    }

    /// The innermost shape or connector under `p` that has a link (draw.io's `link`), itself or
    /// one it is in, and the link.
    pub fn link_at(&self, p: Point, tolerance: f64) -> Option<(CellId, String)> {
        let mut at = self.scene.hit(p, tolerance);
        while let Some(id) = at {
            let link = self.page.cell(id).and_then(|cell| match &cell.value {
                accent_drawio::model::Value::Object { attrs, .. } => {
                    accent_drawio::model::attr(attrs, "link")
                }
                accent_drawio::model::Value::Text(_) => None,
            });
            if let Some(link) = link.filter(|link| !link.is_empty()) {
                return Some((id.to_string(), link.to_string()));
            }
            at = self.parent(id);
        }
        None
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
        (style.get("bendable") != Some("0")).then(|| handle::kind(&style, is_loop))
    }

    /// Where the handle of edge `id`'s label sits, the edge routed along `route`: none for an
    /// edge with no label, or one that cannot be moved.
    pub fn label_handle(&self, id: &str, route: &[Point]) -> Option<Point> {
        let cell = self
            .page
            .cell(id)
            .filter(|c| c.edge && !c.label().is_empty())?;
        let g = cell.geometry.as_ref()?;
        (route.len() > 1 && !self.is_pinned(id))
            .then(|| accent_drawio::scene::edge_label_at(route, g))
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

    /// Attach connector ends within `reach` of a snap point. Otherwise attach to a shape
    /// only if the other endpoint lies outside it: a line drawn inside a background
    /// shape should not route from that shape's far border.
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

    /// The cells a band drawn over the page takes, as draw.io's `selectRegion` takes them: every
    /// cell at any depth whose box lies wholly inside it, a box being what the cell draws and its
    /// rectangle as turned. A group draws nothing of its own, so it comes by its rectangle with
    /// the shapes in it, and a group partly inside gives the shapes the band covers. Nothing
    /// locked or hidden is taken.
    pub fn band(&self, band: Rect) -> Vec<CellId> {
        let drawn = self.drawn();
        let inside = |id: &str, ink: Option<Rect>| {
            let rect = self.rect(id).filter(|r| r.w > 0.0 || r.h > 0.0);
            let turned = rect.map(|r| bounding_box(&r, self.rotation(id)));
            let whole = match (ink, turned) {
                (Some(a), Some(b)) => Some(a.union(&b)),
                (a, b) => a.or(b),
            };
            whole.is_some_and(|r| band.contains_rect(&r))
        };
        self.page
            .cells
            .iter()
            .filter(|c| {
                drawn
                    .get(c.id.as_str())
                    .is_some_and(|ink| inside(&c.id, *ink))
            })
            .map(|c| c.id.clone())
            .collect()
    }

    /// Halfway along edge `id` as it is drawn: where its label goes.
    pub fn edge_middle(&self, id: &str) -> Option<Point> {
        self.scene.prims.iter().find_map(|p| match p {
            Prim::Path { cell, path, .. } if cell == id && self.is_edge(id) => midpoint(path),
            _ => None,
        })
    }

    /// Every cell drawn unlocked at any depth, and the groups those are in, in the page's
    /// order: what Select All takes, as draw.io's takes every cell under a layer
    /// (`selectAll(null, true)`), leaving what is on a locked layer out of a selection.
    pub fn all(&self) -> Vec<CellId> {
        let drawn = self.drawn();
        let cells = self.page.cells.iter();
        let cells = cells.filter(|c| drawn.contains_key(c.id.as_str()));
        cells.map(|c| c.id.clone()).collect()
    }

    /// What each cell draws unlocked, and the groups those cells are in, which draw nothing.
    fn drawn(&self) -> HashMap<&str, Option<Rect>> {
        let mut drawn: HashMap<&str, Option<Rect>> = HashMap::new();
        let prims = self.scene.prims.iter().zip(&self.bounds);
        for (prim, b) in prims.filter(|(p, _)| !p.locked()) {
            let id = prim.cell();
            let ink = drawn.entry(id).or_default();
            *ink = Some(ink.map_or(*b, |r| r.union(b)));
            let mut up = self.parent(id);
            while let Some(group) = up.filter(|g| !drawn.contains_key(*g)) {
                drawn.insert(group, None);
                up = self.parent(group);
            }
        }
        drawn
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zoom::{MAX_SCALE, MIN_SCALE};
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
    fn fitting_shows_the_whole_page_its_width_or_its_height_and_clamps() {
        let fit = |zoom| zoom_scale(zoom, (1000.0, 600.0), (500.0, 500.0));
        assert!((fit(Zoom::Fit) - (500.0 - 2.0 * GAP) / 1000.0).abs() < 1e-9);
        assert_eq!(fit(Zoom::Width), fit(Zoom::Fit));
        assert!((fit(Zoom::Height) - (500.0 - 2.0 * GAP) / 600.0).abs() < 1e-9);
        assert_eq!(zoom_scale(Zoom::Fit, (1.0, 1.0), (1e9, 1e9)), MAX_SCALE);
        assert_eq!(zoom_scale(Zoom::Fit, (1e9, 1e9), (100.0, 100.0)), MIN_SCALE);
    }

    #[test]
    fn with_its_page_view_off_a_page_is_its_drawing() {
        let mut page = Page::blank("P", "p");
        let (w, h) = page.size();
        let blank = Sheet::of(&page, &Context::default());
        let cell = |id: &str, x: f64, y: f64| {
            accent_drawio::Cell::new_vertex(id, "1", Rect::new(x, y, 120.0, 60.0), "", "")
        };
        page.cells.push(cell("a", -400.0, -200.0));
        page.cells.push(cell("b", 200.0, 100.0));
        // On its sheet, the canvas scrolls over the sheet and the drawing, and fits the sheet.
        let sheet = Sheet::of(&page, &Context::default());
        assert_eq!(
            sheet.extent,
            Rect::new(-400.5, -200.5, w + 400.5, h + 200.5)
        );
        assert_eq!(sheet.shown(), Rect::new(0.0, 0.0, w, h));
        // Off it, over the drawing alone (each box grown by half its stroke), and fits that.
        page.set_model_attr("page", Some("0"));
        let sheet = Sheet::of(&page, &Context::default());
        assert_eq!(sheet.page_rect, None);
        assert_eq!(sheet.extent, Rect::new(-400.5, -200.5, 721.0, 361.0));
        assert_eq!(sheet.shown(), sheet.extent);
        // A blank page is the size its sheet would be, either way.
        page.cells.retain(|c| c.id != "a" && c.id != "b");
        assert_eq!(Sheet::of(&page, &Context::default()).extent, blank.extent);
    }

    #[test]
    fn a_wheel_turns_the_page_a_notch_past_the_edge() {
        let mut o = Overshoot::default();
        // The notch that scrolls to the edge turns nothing; the next one, at it, does.
        assert_eq!(o.scroll(1.0, true, false, false, 0), None);
        assert_eq!(o.scroll(1.0, false, false, false, 0), Some(true));
        assert_eq!(o.scroll(-1.0, false, false, false, 0), Some(false));
        // A notch in eighths, as a high-resolution wheel sends one, turns once it is whole.
        for _ in 0..7 {
            assert_eq!(o.scroll(0.125, false, false, false, 0), None);
        }
        assert_eq!(o.scroll(0.125, false, false, false, 0), Some(true));
    }

    #[test]
    fn a_swipe_turns_one_page_and_only_from_the_edge() {
        let mut o = Overshoot::default();
        // A swipe that scrolled to the edge does not read on past it, however far it pushes.
        assert_eq!(o.scroll(30.0, true, true, false, 0), None);
        assert_eq!(o.scroll(300.0, false, true, false, 0), None);
        o = Overshoot::default();
        // One begun at the edge turns once it has pushed far enough, and only once.
        assert_eq!(o.scroll(SWIPE_TURN / 2.0, false, true, false, 0), None);
        assert_eq!(
            o.scroll(SWIPE_TURN / 2.0, false, true, false, 0),
            Some(true)
        );
        assert_eq!(o.scroll(300.0, false, true, false, 0), None);
        o = Overshoot::default();
        assert_eq!(o.scroll(-SWIPE_TURN, false, true, false, 0), Some(false));
    }

    #[test]
    fn a_first_click_stops_beside_a_selected_sibling_and_inside_a_container() {
        let xml = r#"<mxfile><diagram name="P" id="p"><mxGraphModel><root>
            <mxCell id="0"/><mxCell id="1" parent="0"/>
            <mxCell id="g" parent="1" vertex="1" style="group"><mxGeometry x="0" y="0" width="50" height="50" as="geometry"/></mxCell>
            <mxCell id="c" parent="g" vertex="1" style=""><mxGeometry x="0" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="d" parent="g" vertex="1" style=""><mxGeometry x="20" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="s" parent="1" vertex="1" style="swimlane;"><mxGeometry x="100" y="0" width="50" height="50" as="geometry"/></mxCell>
            <mxCell id="e" parent="s" vertex="1" style=""><mxGeometry x="0" y="30" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="k" parent="1" vertex="1" style="container=1;"><mxGeometry x="200" y="0" width="50" height="50" as="geometry"/></mxCell>
            <mxCell id="m" parent="k" vertex="1" style=""><mxGeometry x="0" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="t" parent="1" vertex="1" style="shape=table;childLayout=tableLayout;"><mxGeometry x="300" y="0" width="50" height="50" as="geometry"/></mxCell>
            <mxCell id="r" parent="t" vertex="1" style="shape=tableRow;"><mxGeometry x="0" y="0" width="50" height="25" as="geometry"/></mxCell>
            <mxCell id="x" parent="r" vertex="1" style=""><mxGeometry x="0" y="0" width="25" height="25" as="geometry"/></mxCell>
            <mxCell id="y" parent="r" vertex="1" style=""><mxGeometry x="25" y="0" width="25" height="25" as="geometry"/></mxCell>
            </root></mxGraphModel></diagram></mxfile>"#;
        let file = accent_drawio::File::from_bytes(xml.as_bytes()).unwrap();
        let sheet = Sheet::of(&file.pages()[0], &Context::default());
        let ids = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        let first = |p: Point, selected: &[&str]| sheet.pick(p, 1.0, &ids(selected)).unwrap().cell;
        let on_d = Point::new(25.0, 5.0);
        // A group's shape is reached through the group.
        assert_eq!(first(on_d, &[]), "g");
        // With its sibling selected, at once, as draw.io switches between a group's shapes.
        assert_eq!(first(on_d, &["c"]), "d");
        // A swimlane's and a container's shapes at once.
        assert_eq!(first(Point::new(105.0, 35.0), &[]), "e");
        assert_eq!(first(Point::new(205.0, 5.0), &[]), "m");
        // A table's cell goes to its table, a sibling selected or not.
        assert_eq!(first(Point::new(330.0, 10.0), &[]), "t");
        assert_eq!(first(Point::new(330.0, 10.0), &["x"]), "t");
    }

    #[test]
    fn a_first_click_climbs_as_draw_io_s_select_parent_first_transparent_bounds_and_labels_say() {
        let xml = r#"<mxfile><diagram name="P" id="p"><mxGraphModel><root>
            <mxCell id="0"/><mxCell id="1" parent="0"/>
            <mxCell id="s" parent="1" vertex="1" style="swimlane;selectParentFirst=1;"><mxGeometry x="0" y="0" width="50" height="50" as="geometry"/></mxCell>
            <mxCell id="e" parent="s" vertex="1" style=""><mxGeometry x="0" y="30" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="f" parent="s" vertex="1" style=""><mxGeometry x="20" y="30" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="b" parent="1" vertex="1" style="group;transparentBounds=1;"><mxGeometry as="geometry"/></mxCell>
            <mxCell id="c" parent="b" vertex="1" style=""><mxGeometry x="100" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="x" parent="1" edge="1" style=""><mxGeometry relative="1" as="geometry"><mxPoint x="200" y="0" as="sourcePoint"/><mxPoint x="200" y="50" as="targetPoint"/></mxGeometry></mxCell>
            <mxCell id="l" value="Label" parent="x" vertex="1" style="edgeLabel;"><mxGeometry relative="1" as="geometry"><mxPoint as="offset"/></mxGeometry></mxCell>
            </root></mxGraphModel></diagram></mxfile>"#;
        let file = accent_drawio::File::from_bytes(xml.as_bytes()).unwrap();
        let sheet = Sheet::of(&file.pages()[0], &Context::default());
        let ids = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        let first = |p: Point, selected: &[&str]| sheet.pick(p, 1.0, &ids(selected)).unwrap().cell;
        // A swimlane asking to be selected first is, though it holds its shapes, but not while
        // one of them is selected.
        assert_eq!(first(Point::new(5.0, 35.0), &[]), "s");
        assert_eq!(first(Point::new(5.0, 35.0), &["f"]), "e");
        // A shape in a group whose bounds are its shapes' at once.
        assert_eq!(first(Point::new(105.0, 5.0), &[]), "c");
        // A connector's own label is its own.
        assert!(!sheet.climbs("l", &[]));
    }

    #[test]
    fn a_first_click_stays_in_the_group_that_holds_the_selection() {
        let xml = r#"<mxfile><diagram name="P" id="p"><mxGraphModel><root>
            <mxCell id="0"/><mxCell id="1" parent="0"/>
            <mxCell id="G" parent="1" vertex="1" style="group"><mxGeometry x="0" y="0" width="100" height="10" as="geometry"/></mxCell>
            <mxCell id="g1" parent="G" vertex="1" style="group"><mxGeometry x="0" y="0" width="30" height="10" as="geometry"/></mxCell>
            <mxCell id="c1" parent="g1" vertex="1" style=""><mxGeometry x="0" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="c2" parent="g1" vertex="1" style=""><mxGeometry x="20" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="g2" parent="G" vertex="1" style="group"><mxGeometry x="50" y="0" width="30" height="10" as="geometry"/></mxCell>
            <mxCell id="d1" parent="g2" vertex="1" style=""><mxGeometry x="0" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="d2" parent="g2" vertex="1" style=""><mxGeometry x="20" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="a" parent="1" vertex="1" style=""><mxGeometry x="200" y="0" width="10" height="10" as="geometry"/></mxCell>
            </root></mxGraphModel></diagram></mxfile>"#;
        let file = accent_drawio::File::from_bytes(xml.as_bytes()).unwrap();
        let sheet = Sheet::of(&file.pages()[0], &Context::default());
        let ids = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        let first = |p: Point, selected: &[&str]| sheet.pick(p, 1.0, &ids(selected)).unwrap().cell;
        let on_d1 = Point::new(55.0, 5.0);
        // With a shape of g1 selected, G holds the selection: a click on d1 opens g2 beside g1,
        // where draw.io would go back out to G.
        assert_eq!(first(on_d1, &["c1"]), "g2");
        assert_eq!(first(on_d1, &["g1"]), "g2");
        // Beside a selected sibling as before, and from the outermost group with nothing of it
        // selected.
        assert_eq!(first(Point::new(25.0, 5.0), &["c1"]), "c2");
        assert_eq!(first(on_d1, &["a"]), "G");
    }

    #[test]
    fn a_band_takes_every_cell_it_covers_a_group_with_its_shapes() {
        let xml = r#"<mxfile><diagram name="P" id="p"><mxGraphModel><root>
            <mxCell id="0"/><mxCell id="1" parent="0"/>
            <mxCell id="g" parent="1" vertex="1" style="group"><mxGeometry x="0" y="0" width="30" height="10" as="geometry"/></mxCell>
            <mxCell id="c" parent="g" vertex="1" style=""><mxGeometry x="0" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="d" parent="g" vertex="1" style=""><mxGeometry x="20" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="h" parent="1" vertex="1" style="" visible="0"><mxGeometry x="40" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="2" parent="0" style="locked=1;"/>
            <mxCell id="k" parent="2" vertex="1" style=""><mxGeometry x="60" y="0" width="10" height="10" as="geometry"/></mxCell>
            </root></mxGraphModel></diagram></mxfile>"#;
        let file = accent_drawio::File::from_bytes(xml.as_bytes()).unwrap();
        let sheet = Sheet::of(&file.pages()[0], &Context::default());
        let ids = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        // A group by its rectangle, with its shapes; nothing hidden or on a locked layer.
        assert_eq!(
            sheet.band(Rect::new(-5.0, -5.0, 80.0, 20.0)),
            ids(&["g", "c", "d"])
        );
        // Of a group partly inside, the shapes wholly inside.
        assert_eq!(sheet.band(Rect::new(-5.0, -5.0, 20.0, 20.0)), ids(&["c"]));
        assert_eq!(sheet.band(Rect::new(-5.0, -5.0, 30.0, 20.0)), ids(&["c"]));
    }

    #[test]
    fn a_click_on_a_selected_group_s_empty_middle_opens_it() {
        let xml = r#"<mxfile><diagram name="P" id="p"><mxGraphModel><root>
            <mxCell id="0"/><mxCell id="1" parent="0"/>
            <mxCell id="G" parent="1" vertex="1" style="group"><mxGeometry x="0" y="0" width="100" height="10" as="geometry"/></mxCell>
            <mxCell id="g" parent="G" vertex="1" style="group"><mxGeometry x="0" y="0" width="30" height="10" as="geometry"/></mxCell>
            <mxCell id="c" parent="g" vertex="1" style=""><mxGeometry x="0" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="d" parent="g" vertex="1" style=""><mxGeometry x="20" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="h" parent="G" vertex="1" style="" visible="0"><mxGeometry x="40" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="e" parent="G" vertex="1" style=""><mxGeometry x="90" y="0" width="10" height="10" as="geometry"/></mxCell>
            </root></mxGraphModel></diagram></mxfile>"#;
        let file = accent_drawio::File::from_bytes(xml.as_bytes()).unwrap();
        let sheet = Sheet::of(&file.pages()[0], &Context::default());
        let ids = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        let open = |p: Point, selected: &[&str]| sheet.opened_at(p, &ids(selected));
        let (in_g, in_g_only) = (Point::new(15.0, 5.0), Point::new(60.0, 5.0));
        // What is shown in it, one level in.
        assert_eq!(open(in_g_only, &["G"]), Some(ids(&["g", "e"])));
        // The innermost selected group the click is in.
        assert_eq!(open(in_g, &["G", "g"]), Some(ids(&["c", "d"])));
        // Outside every group selected, nothing.
        assert_eq!(open(in_g_only, &["g"]), None);
        assert_eq!(open(Point::new(200.0, 5.0), &["G"]), None);
    }

    #[test]
    fn a_group_is_open_while_the_selection_is_inside_it() {
        let xml = r#"<mxfile><diagram name="P" id="p"><mxGraphModel><root>
            <mxCell id="0"/><mxCell id="1" parent="0"/>
            <mxCell id="G" parent="1" vertex="1" style="group"><mxGeometry x="0" y="0" width="100" height="10" as="geometry"/></mxCell>
            <mxCell id="g" parent="G" vertex="1" style="group"><mxGeometry x="0" y="0" width="30" height="10" as="geometry"/></mxCell>
            <mxCell id="c" parent="g" vertex="1" style=""><mxGeometry x="0" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="d" parent="g" vertex="1" style=""><mxGeometry x="20" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="e" parent="G" vertex="1" style=""><mxGeometry x="90" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="a" parent="1" vertex="1" style=""><mxGeometry x="200" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="t" parent="1" vertex="1" style="shape=table;childLayout=tableLayout;"><mxGeometry x="300" y="0" width="50" height="25" as="geometry"/></mxCell>
            <mxCell id="r" parent="t" vertex="1" style="shape=tableRow;"><mxGeometry x="0" y="0" width="50" height="25" as="geometry"/></mxCell>
            <mxCell id="x" parent="r" vertex="1" style=""><mxGeometry x="0" y="0" width="25" height="25" as="geometry"/></mxCell>
            </root></mxGraphModel></diagram></mxfile>"#;
        let file = accent_drawio::File::from_bytes(xml.as_bytes()).unwrap();
        let sheet = Sheet::of(&file.pages()[0], &Context::default());
        let ids = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        let open = |selected: &[&str]| sheet.open_group(&ids(selected));
        assert_eq!(open(&["c"]).as_deref(), Some("g"));
        assert_eq!(open(&["c", "d"]).as_deref(), Some("g"));
        assert_eq!(open(&["c", "e"]).as_deref(), Some("G"));
        // A group selected is not open itself, the one holding it is.
        assert_eq!(open(&["g"]).as_deref(), Some("G"));
        assert_eq!(open(&["G"]), None);
        assert_eq!(open(&["c", "a"]), None);
        assert_eq!(open(&[]), None);
        // A table's cell opens neither its row nor its table.
        assert_eq!(open(&["x"]), None);
    }

    #[test]
    fn an_edited_page_is_pulled_three_notches_before_it_turns_and_a_spin_turns_on() {
        let mut o = Overshoot::default();
        // Wheel notches, each at its event time in ms.
        assert_eq!(o.scroll(1.0, false, false, true, 0), None);
        assert!((o.pull(false) - PULL_PX / 3.0).abs() < 1e-9);
        // A push the other way lets go at once and pulls the other way.
        assert_eq!(o.scroll(-1.0, false, false, true, 50), None);
        assert!((o.pull(false) + PULL_PX / 3.0).abs() < 1e-9);
        assert_eq!(o.scroll(-1.0, false, false, true, 100), None);
        assert_eq!(o.scroll(-1.0, false, false, true, 150), Some(false));
        assert_eq!(o.pull(false), 0.0);
        // The spin goes on, turning a page every third notch.
        assert_eq!(o.scroll(-1.0, false, false, true, 200), None);
        assert_eq!(o.scroll(-1.0, false, false, true, 250), None);
        assert_eq!(o.scroll(-1.0, false, false, true, 300), Some(false));
        // Notches 450 ms apart add up across the spring back between them.
        assert_eq!(o.scroll(-1.0, false, false, true, 750), None);
        assert_eq!(o.scroll(-1.0, false, false, true, 1200), None);
        assert!((o.pull(false) + 2.0 * PULL_PX / 3.0).abs() < 1e-9);
        assert_eq!(o.scroll(-1.0, false, false, true, 1650), Some(false));
        // Notches over a second apart each start again.
        assert_eq!(o.scroll(-1.0, false, false, true, 2700), None);
        assert_eq!(o.scroll(-1.0, false, false, true, 3800), None);
        assert!((o.pull(false) + PULL_PX / 3.0).abs() < 1e-9);
        // A swipe turns at three times a presented page's, and only once.
        let mut o = Overshoot::default();
        assert_eq!(o.scroll(SWIPE_TURN * 2.0, false, true, true, 0), None);
        assert_eq!(o.scroll(SWIPE_TURN, false, true, true, 16), Some(true));
        assert_eq!(o.scroll(SWIPE_TURN * 3.0, false, true, true, 32), None);
    }

    #[test]
    fn the_grid_is_halved_zoomed_in_and_doubled_zoomed_far_out() {
        assert_eq!(grid_step(10.0, 1.0), 10.0);
        assert_eq!(grid_step(10.0, 1.6), 5.0);
        assert_eq!(grid_step(10.0, 4.0), 2.5);
        assert_eq!(grid_step(10.0, 0.5), 10.0);
        assert_eq!(grid_step(10.0, 0.25), 20.0);
        assert_eq!(grid_step(10.0, 0.1), 40.0);
    }

    #[test]
    fn a_move_lands_the_origin_on_the_grid() {
        let d = snap_move(Point::new(13.0, 20.0), Point::new(5.0, 4.0), 10.0);
        assert_eq!((d.x, d.y), (7.0, 0.0));
        assert_eq!(snap(14.9, 10.0), 10.0);
    }

    #[test]
    fn a_locked_move_keeps_to_the_axis_it_goes_furthest_along() {
        assert_eq!(move_axes(Point::new(30.0, -10.0), true), [true, false]);
        assert_eq!(move_axes(Point::new(5.0, -20.0), true), [false, true]);
        assert_eq!(move_axes(Point::new(5.0, -20.0), false), [true, true]);
    }

    #[test]
    fn a_line_s_end_keeps_to_steps_of_fifteen_degrees_and_its_length_to_the_grid() {
        let o = Point::new(10.0, 10.0);
        let flat = snap_angle(o, Point::new(110.0, 16.0), None);
        assert_eq!(flat, Point::new(110.18, 10.0));
        let down = snap_angle(o, Point::new(13.0, 108.0), Some(10.0));
        assert_eq!(down, Point::new(10.0, 110.0));
        let back = snap_angle(o, Point::new(-40.0, 11.0), Some(10.0));
        assert_eq!(back, Point::new(-40.0, 10.0));
        // 44° is 45°, its length on the grid; 30° with its length kept.
        let d = snap_angle(o, Point::new(80.0, 78.0), Some(10.0));
        assert_eq!(d, Point::new(80.71, 80.71));
        let d = snap_angle(o, Point::new(96.0, 59.0), None);
        assert_eq!(d, Point::new(95.72, 59.49));
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
    fn a_click_on_what_is_selected_selects_the_cell_under_it_alone() {
        let xml = r#"<mxfile><diagram name="P" id="p"><mxGraphModel><root>
            <mxCell id="0"/><mxCell id="1" parent="0"/>
            <mxCell id="a" parent="1" vertex="1" style=""><mxGeometry x="0" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="g" parent="1" vertex="1" style="group"><mxGeometry x="100" y="0" width="50" height="50" as="geometry"/></mxCell>
            <mxCell id="c" parent="g" vertex="1" style=""><mxGeometry x="0" y="0" width="10" height="10" as="geometry"/></mxCell>
            </root></mxGraphModel></diagram></mxfile>"#;
        let file = accent_drawio::File::from_bytes(xml.as_bytes()).unwrap();
        let sheet = Sheet::of(&file.pages()[0], &Context::default());
        let ids = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        let click = |p: Point, selected: &[&str]| {
            let selection = ids(selected);
            let pick = sheet.pick(p, 1.0, &selection).unwrap();
            pick.clicked(&selection)
        };
        let (on_a, on_c) = (Point::new(5.0, 5.0), Point::new(105.0, 5.0));
        // One level into the selected group.
        assert_eq!(click(on_c, &["g"]), Some(ids(&["c"])));
        // One of several selected, a group's shape one level into it.
        assert_eq!(click(on_a, &["a", "g"]), Some(ids(&["a"])));
        assert_eq!(click(on_c, &["a", "g"]), Some(ids(&["c"])));
        // The one cell selected stays selected.
        assert_eq!(click(on_a, &["a"]), None);
    }

    #[test]
    fn a_selection_frame_sits_a_unit_outside_the_line() {
        let mut page = Page::blank("P", "p");
        let r = Rect::new(0.0, 0.0, 40.0, 20.0);
        for (id, style) in [
            ("plain", ""),
            ("thick", "strokeWidth=4;"),
            ("bare", "strokeColor=none;"),
        ] {
            page.cells
                .push(accent_drawio::Cell::new_vertex(id, "1", r, style, ""));
        }
        let (a, b) = (Point::new(100.0, 0.0), Point::new(200.0, 0.0));
        page.cells.push(accent_drawio::Cell::new_edge(
            "e",
            "1",
            (None, a),
            (None, b),
            "",
        ));
        let sheet = Sheet::of(&page, &Context::default());
        let pads = ["plain", "thick", "bare", "e"].map(|id| sheet.padding(id));
        assert_eq!(pads, [1.5, 3.0, 1.0, 1.0]);
    }

    #[test]
    fn a_line_turns_unless_an_end_is_on_a_shape() {
        let mut page = Page::blank("P", "p");
        let r = Rect::new(0.0, 0.0, 40.0, 20.0);
        page.cells
            .push(accent_drawio::Cell::new_vertex("s", "1", r, "", ""));
        let edge = |id: &str, on: Option<&str>| {
            let (a, b) = (Point::new(100.0, 0.0), Point::new(200.0, 0.0));
            accent_drawio::Cell::new_edge(id, "1", (on, a), (None, b), "")
        };
        page.cells.push(edge("line", None));
        page.cells.push(edge("arrow", Some("s")));
        let sheet = Sheet::of(&page, &Context::default());
        assert!(sheet.is_turnable("s") && sheet.is_turnable("line"));
        assert!(!sheet.is_turnable("arrow"));
    }

    #[test]
    fn a_locked_layer_pins_its_cells_and_select_all_leaves_them_out() {
        let xml = r#"<mxfile><diagram name="P" id="p"><mxGraphModel><root>
            <mxCell id="0"/><mxCell id="L" parent="0" style="locked=1;"/><mxCell id="C" parent="0"/>
            <mxCell id="a" parent="L" vertex="1" style=""><mxGeometry x="0" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="b" parent="C" vertex="1" style=""><mxGeometry x="20" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="g" parent="C" vertex="1" style="group"><mxGeometry x="40" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="c" parent="g" vertex="1" style=""><mxGeometry x="0" y="0" width="10" height="10" as="geometry"/></mxCell>
            </root></mxGraphModel></diagram></mxfile>"#;
        let file = accent_drawio::File::from_bytes(xml.as_bytes()).unwrap();
        let sheet = Sheet::of(&file.pages()[0], &Context::default());
        assert!(sheet.is_pinned("a") && !sheet.is_pinned("b"));
        // A group and what it holds, as draw.io's Select All takes them.
        assert_eq!(sheet.all(), ["b", "g", "c"]);
    }

    #[test]
    fn a_group_sized_by_what_it_holds_is_framed_round_it_and_left_by_a_second_click() {
        let xml = r#"<mxfile><diagram name="P" id="p"><mxGraphModel><root>
            <mxCell id="0"/><mxCell id="1" parent="0"/>
            <mxCell id="b" parent="1" vertex="1" style="group;transparentBounds=1;"><mxGeometry as="geometry"/></mxCell>
            <mxCell id="c" parent="b" vertex="1" style=""><mxGeometry x="100" y="0" width="10" height="10" as="geometry"/></mxCell>
            <mxCell id="d" parent="b" vertex="1" style=""><mxGeometry x="130" y="20" width="10" height="10" as="geometry"/></mxCell>
            </root></mxGraphModel></diagram></mxfile>"#;
        let file = accent_drawio::File::from_bytes(xml.as_bytes()).unwrap();
        let sheet = Sheet::of(&file.pages()[0], &Context::default());
        assert_eq!(sheet.rect("b"), Some(Rect::new(100.0, 0.0, 40.0, 30.0)));
        assert!(!sheet.is_resizable("b") && !sheet.is_turnable("b"));
        let ids = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        let click = |selected: &[&str]| {
            let pick = sheet.pick(Point::new(105.0, 5.0), 1.0, &ids(selected));
            pick.unwrap().clicked(&ids(selected))
        };
        assert_eq!(click(&["c"]), Some(ids(&["b"])));
        assert_eq!(click(&["b"]), Some(ids(&["c"])), "and back in");
        assert_eq!(
            click(&["c", "d"]),
            Some(ids(&["c"])),
            "one of several stays itself"
        );
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
        assert_eq!(
            rotation_to(c, up, Point::new(50.0, 0.0), false, false),
            90.0
        );
        // Near the circle, 15° steps; well outside it, whole degrees; free, tenths.
        let near = Point::new(50.0, -8.0);
        assert_eq!(rotation_to(c, up, near, false, false), 75.0);
        let far = Point::new(200.0, -32.0);
        assert_eq!(rotation_to(c, up, far, false, false), 81.0);
        assert_eq!(rotation_to(c, up, far, true, false), 80.9);
        // Shift keeps to 15° wherever the pointer is, Alt too.
        assert_eq!(rotation_to(c, up, far, false, true), 75.0);
        assert_eq!(rotation_to(c, up, far, true, true), 75.0);
        // A handle to the upper right held straight down is turned past a half: -135°, not 225°.
        let corner = Point::new(50.0, -50.0);
        assert_eq!(
            rotation_to(c, corner, Point::new(-50.0, 50.0), false, false),
            180.0
        );
        assert_eq!(
            rotation_to(c, corner, Point::new(-50.0, 0.0), false, false),
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
    fn a_shape_with_fixed_proportions_keeps_them_from_any_handle() {
        let r = Rect::new(0.0, 0.0, 40.0, 20.0);
        let resize =
            |h, dx, dy| keep_proportions(resize_by(&r, h, Point::new(dx, dy), None), &r, h);
        assert_eq!(
            resize(Handle::East, 40.0, 0.0),
            Rect::new(0.0, 0.0, 80.0, 40.0)
        );
        assert_eq!(
            resize(Handle::South, 0.0, 10.0),
            Rect::new(0.0, 0.0, 60.0, 30.0)
        );
        let nw = resize(Handle::NorthWest, -40.0, 0.0);
        assert_eq!(nw, Rect::new(-40.0, -20.0, 80.0, 40.0));
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
