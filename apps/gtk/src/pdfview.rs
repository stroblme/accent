//! The widget a PDF is read in: a scrollable column of pages painted from cached tiles.
//!
//! Nothing here opens a file or calls pdfium. The widget knows the page sizes, the zoom and the
//! textures it has been given; when it paints a page it has no tiles for, it asks for them and
//! draws a blurred low-resolution stand-in until they arrive. `pdftab.rs` owns the document and
//! the thread that answers.
//!
//! Two of these share one document: the reading view, and a narrow one beside it showing every
//! page as a thumbnail.

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

/// Tile edge in device pixels. 512 is 1 MiB of RGBA, small enough that a scroll never waits on
/// one page-sized render and large enough that a screen is a handful of them.
pub const TILE: i32 = 512;

/// Width of the low-resolution stand-in, in device pixels. Also what the thumbnail strip paints.
pub const LOWRES_W: i32 = 256;

/// Between pages, and around the column. The 12 of DESIGN.md's spacing scale.
const GAP: f32 = 12.0;

/// Tile bytes held before the least recently used are dropped.
const BUDGET: usize = 256 << 20;

/// The same for the low-resolution stand-ins, which used to be kept for the life of the tab: at
/// 370 KB each (256 x 362 x 4 for A4) a 500-page document strip-scrolled from end to end held
/// 177 MB of them, and twice that once the reader had seen it in both light and dark.
///
/// A quarter of [`BUDGET`], which is about 180 A4 pages. The most that can be on screen at once
/// is far less: the reading view paints one viewport of prefetch either side of the one being
/// read, which at the 10 % minimum zoom and a 2 000 px-tall viewport is 48 pages, and the strip
/// beside it another 20. Eviction therefore never reaches a page either view is painting.
const LOWRES_BUDGET: usize = 64 << 20;

/// Points to CSS pixels at zoom 1.0. A PDF point is 1/72 inch and a CSS pixel 1/96.
const PT_TO_PX: f32 = 96.0 / 72.0;

/// What a page may be zoomed between, and what an image tab borrows: 10 % is a letter page
/// about 80 px wide, and past 800 % one page is more tiles than the budget holds.
/// How much of the page a highlighter lets through. It also multiplies rather than covers, so
/// this is about how strong the colour is, not about whether the text survives.
pub const HIGHLIGHTER_ALPHA: f32 = 0.4;

/// What a drag over the page does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Select the text under it, which is what a drag has always done.
    #[default]
    Select,
    /// Draw on the page.
    Pen,
    /// Draw over it in a wide translucent stroke that darkens rather than covers.
    Highlighter,
    /// Take a stroke off it.
    Eraser,
    /// A straight line, snapped to the axis it is close to.
    Line,
    /// A rectangle between the press and the release.
    Rect,
    /// A circle grown from the press outwards.
    Circle,
    /// Take hold of a stroke: drag its middle to move it, an edge to stretch it, a corner to
    /// scale it.
    Adjust,
}

impl Mode {
    /// Whether a drag draws — everything but selecting, erasing and adjusting.
    pub fn draws(self) -> bool {
        matches!(
            self,
            Mode::Pen | Mode::Highlighter | Mode::Line | Mode::Rect | Mode::Circle
        )
    }

    /// Whether a drag is a shape: two points, the press and the release, rather than a path.
    pub fn shapes(self) -> bool {
        matches!(self, Mode::Line | Mode::Rect | Mode::Circle)
    }
}

pub const MIN_SCALE: f64 = 0.1;
pub const MAX_SCALE: f64 = 8.0;

/// How the page is sized to the window. Defined in core, because the session remembers it.
pub use accent_core::config::PdfZoom;

/// The status bar's readout for a zoom. A PDF always has one, so there is always something to
/// click to get back to Fit Width.
pub fn zoom_label(zoom: PdfZoom) -> Option<String> {
    match zoom {
        PdfZoom::FitWidth => Some("Fit Width".to_string()),
        PdfZoom::FitPage => Some("Fit Page".to_string()),
        PdfZoom::Scale(z) => Some(format!("{} %", (z * 100.0).round() as i32)),
    }
}

/// Where a page sits in the scrolled content, in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Every page's place at one scale, plus the size of the content they add up to.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    /// CSS pixels per PDF point.
    pub scale: f32,
    pub pages: Vec<PageRect>,
    pub width: f32,
    pub height: f32,
}

/// A reading position that survives a zoom, a resize and a reload: which page, and the fraction
/// of it at the top-left of the viewport.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    pub page: usize,
    pub u: f32,
    pub v: f32,
}

impl Default for Anchor {
    fn default() -> Self {
        Anchor {
            page: 0,
            u: 0.0,
            v: 0.0,
        }
    }
}

impl Anchor {
    /// The same place in a document that has since gained or lost pages.
    pub fn clamped(self, pages: usize) -> Anchor {
        Anchor {
            page: self.page.min(pages.saturating_sub(1)),
            ..self
        }
    }
}

/// The two ends of a drag, each a page and a point on it in that page's own points.
///
/// The two need not be the same page, and `to` may be earlier in the document than `from`: a
/// drag runs in whichever direction the reader pulls it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub from: (usize, (f32, f32)),
    pub to: (usize, (f32, f32)),
}

/// One tile of one page at one scale, in one colour scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub page: u32,
    /// Device pixels per point times 1000, so a float scale can be a hash key.
    pub scale_milli: u32,
    pub tx: u16,
    pub ty: u16,
    pub dark: bool,
}

/// Textures already rendered, dropped least-recently-used first once they outgrow their budget.
///
/// Two maps and two budgets, because the two kinds of texture are wanted for different lengths of
/// time: a tile is one square of one page at one zoom and is stale the moment the zoom changes,
/// while a stand-in is a whole page at a fixed size and stays useful at every zoom.
#[derive(Default)]
pub struct Cache {
    tiles: HashMap<TileKey, (gdk::MemoryTexture, u64)>,
    /// One whole-page thumbnail per page and theme, which is what a page shows before its tiles
    /// arrive and what the thumbnail strip paints, under [`LOWRES_BUDGET`].
    lowres: HashMap<(u32, bool), (gdk::MemoryTexture, u64)>,
    bytes: usize,
    lowres_bytes: usize,
    tick: u64,
}

impl Cache {
    pub fn get(&mut self, key: &TileKey) -> Option<gdk::MemoryTexture> {
        self.tick += 1;
        let tick = self.tick;
        let (texture, used) = self.tiles.get_mut(key)?;
        *used = tick;
        Some(texture.clone())
    }

    pub fn insert(&mut self, key: TileKey, texture: gdk::MemoryTexture, bytes: usize) {
        self.tick += 1;
        if let Some((_, old)) = self.tiles.insert(key, (texture, self.tick)) {
            let _ = old;
        }
        self.bytes += bytes;
        self.bytes -= drop_oldest(&mut self.tiles, self.bytes, BUDGET);
    }

    /// Takes `&mut self` so that painting a page counts as using its stand-in: eviction is by
    /// least recently *painted*, which is what keeps what is on screen off the list.
    pub fn lowres(&mut self, page: u32, dark: bool) -> Option<gdk::MemoryTexture> {
        self.tick += 1;
        let tick = self.tick;
        let (texture, used) = self.lowres.get_mut(&(page, dark))?;
        *used = tick;
        Some(texture.clone())
    }

    pub fn insert_lowres(&mut self, page: u32, dark: bool, texture: gdk::MemoryTexture) {
        self.tick += 1;
        self.lowres_bytes += (texture.width() * texture.height() * 4) as usize;
        if let Some((old, _)) = self.lowres.insert((page, dark), (texture, self.tick)) {
            self.lowres_bytes -= (old.width() * old.height() * 4) as usize;
        }
        self.lowres_bytes -= drop_oldest(&mut self.lowres, self.lowres_bytes, LOWRES_BUDGET);
    }

    pub fn clear(&mut self) {
        self.tiles.clear();
        self.lowres.clear();
        self.bytes = 0;
        self.lowres_bytes = 0;
    }

    /// Forget what was rendered of one page, **except** its tiles at the scale and scheme now on
    /// screen, which stay to be painted while their replacements render.
    ///
    /// What a stroke or an exported highlight invalidates: the page it landed on is drawn
    /// differently now and every other page is exactly as it was, so dropping the whole cache
    /// would re-render the viewport and its prefetch after every stroke. What is kept is stale
    /// and is asked for again — see [`PdfView::refresh_page`] — but painting yesterday's render
    /// of a page for the 30 ms its replacement takes is invisible, where painting blank paper is
    /// the flash this exists to avoid. Everything else is going spare: nobody is looking at a
    /// render at another zoom, and keeping it would paint the old page after the next one.
    pub fn forget_page_except(&mut self, page: u32, scale_milli: u32, dark: bool) {
        let (tiles, lowres) = (&mut self.bytes, &mut self.lowres_bytes);
        self.tiles.retain(|key, (texture, _)| {
            let keep = key.page != page || (key.scale_milli == scale_milli && key.dark == dark);
            if !keep {
                *tiles -= bytes_of(texture);
            }
            keep
        });
        self.lowres.retain(|(at, _), (texture, _)| {
            let keep = *at != page;
            if !keep {
                *lowres -= bytes_of(texture);
            }
            keep
        });
    }
}

/// Drop the least recently used entries of `map` until it is comfortably under `budget`, and
/// report how many bytes that freed.
fn drop_oldest<K: Copy + Eq + std::hash::Hash>(
    map: &mut HashMap<K, (gdk::MemoryTexture, u64)>,
    bytes: usize,
    budget: usize,
) -> usize {
    let used = map
        .iter()
        .map(|(key, (texture, tick))| (*tick, bytes_of(texture), *key))
        .collect();
    let mut freed = 0;
    for key in overflowing(used, bytes, budget) {
        if let Some((texture, _)) = map.remove(&key) {
            freed += bytes_of(&texture);
        }
    }
    freed
}

/// Which entries a cache of `bytes` has to give up to come back comfortably under `budget`:
/// the least recently used first, down to three quarters of it rather than to the line, so
/// eviction happens in batches rather than on every insert once the cache is full.
///
/// `used` is every entry as its tick, its size and its key. Kept apart from the textures so the
/// policy can be checked without a display.
fn overflowing<K: Copy>(mut used: Vec<(u64, usize, K)>, bytes: usize, budget: usize) -> Vec<K> {
    if bytes <= budget {
        return Vec::new();
    }
    used.sort_unstable_by_key(|(tick, _, _)| *tick);
    let mut left = bytes;
    let mut out = Vec::new();
    for (_, size, key) in used {
        if left * 4 <= budget * 3 {
            break;
        }
        left -= size;
        out.push(key);
    }
    out
}

fn bytes_of(texture: &gdk::MemoryTexture) -> usize {
    (texture.width() * texture.height() * 4) as usize
}

/// A tile the widget wants and does not have. `u16::MAX` in both axes means the whole page at
/// low resolution, which is what it paints while the real tiles are still being rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Want {
    pub page: u32,
    pub tx: u16,
    pub ty: u16,
}

impl Want {
    pub fn is_lowres(&self) -> bool {
        self.tx == u16::MAX
    }
}

/// How close to an axis, in degrees, a line has to be to snap onto it.
const SNAP_DEG: f32 = 7.0;
/// How near the pointer has to come to a stroke, in page points, for the Adjust tool to take it.
const ADJUST_RADIUS: f32 = 4.0;
/// The side of an Adjust handle, in pixels.
const HANDLE: f32 = 8.0;

/// A line's end pulled onto the axis through its start when it is within [`SNAP_DEG`] of one.
pub fn snap(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let angle = dy.abs().atan2(dx.abs()).to_degrees();
    if angle < SNAP_DEG {
        (b.0, a.1)
    } else if angle > 90.0 - SNAP_DEG {
        (a.0, b.1)
    } else {
        b
    }
}

/// The shape a drag from `a` to `b` means under `mode`, or nothing for a drag too short to be
/// one — a click in a shape mode draws nothing — and for any other mode.
pub fn shape_of(mode: Mode, a: (f32, f32), b: (f32, f32)) -> Option<accent_core::pdf::Shape> {
    use accent_core::pdf::Shape;
    let radius = (b.0 - a.0).hypot(b.1 - a.1);
    if radius < 1.0 {
        return None;
    }
    match mode {
        Mode::Line => Some(Shape::Line { a, b }),
        Mode::Rect => Some(Shape::Rect(accent_core::pdf::Rect {
            left: a.0.min(b.0),
            top: a.1.min(b.1),
            right: a.0.max(b.0),
            bottom: a.1.max(b.1),
        })),
        Mode::Circle => Some(Shape::Circle { centre: a, radius }),
        _ => None,
    }
}

/// Where on a selected stroke's box the Adjust tool took hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    Move,
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// Which part of `bounds` is under `at`: a corner within `grip` of it, else an edge, else the
/// middle, else nothing.
pub fn handle_at(bounds: accent_core::pdf::Rect, at: (f32, f32), grip: f32) -> Option<Handle> {
    let near = |v: f32, edge: f32| (v - edge).abs() <= grip;
    let inside = at.0 >= bounds.left - grip
        && at.0 <= bounds.right + grip
        && at.1 >= bounds.top - grip
        && at.1 <= bounds.bottom + grip;
    if !inside {
        return None;
    }
    let (l, r) = (near(at.0, bounds.left), near(at.0, bounds.right));
    let (t, b) = (near(at.1, bounds.top), near(at.1, bounds.bottom));
    Some(match (l, r, t, b) {
        (true, _, true, _) => Handle::TopLeft,
        (_, true, true, _) => Handle::TopRight,
        (true, _, _, true) => Handle::BottomLeft,
        (_, true, _, true) => Handle::BottomRight,
        (true, ..) => Handle::Left,
        (_, true, ..) => Handle::Right,
        (_, _, true, _) => Handle::Top,
        (_, _, _, true) => Handle::Bottom,
        _ => Handle::Move,
    })
}

/// The map a drag of `handle` by `(dx, dy)` page points applies to a stroke with this box.
///
/// The middle translates. An edge stretches its axis about the opposite edge. A corner scales
/// both axes alike, by the drag's projection onto the diagonal, about the opposite corner. A
/// scale never drops below 0.05, and an axis with no extent — a horizontal line's height — is
/// left alone rather than divided by.
pub fn drag_matrix(
    handle: Handle,
    b: accent_core::pdf::Rect,
    dx: f32,
    dy: f32,
) -> accent_core::pdf::Matrix {
    let (w, h) = (b.width(), b.height());
    let factor = |delta: f32, extent: f32| match extent > f32::EPSILON {
        true => (1.0 + delta / extent).max(0.05),
        false => 1.0,
    };
    let diagonal = |dx: f32, dy: f32| match w * w + h * h {
        d2 if d2 > f32::EPSILON => (1.0 + (dx * w + dy * h) / d2).max(0.05),
        _ => 1.0,
    };
    // Scaling about a fixed line: x' = s·x + (1 − s)·fixed.
    let about =
        |sx: f32, sy: f32, fx: f32, fy: f32| [sx, 0.0, 0.0, sy, (1.0 - sx) * fx, (1.0 - sy) * fy];
    match handle {
        Handle::Move => [1.0, 0.0, 0.0, 1.0, dx, dy],
        Handle::Right => about(factor(dx, w), 1.0, b.left, 0.0),
        Handle::Left => about(factor(-dx, w), 1.0, b.right, 0.0),
        Handle::Bottom => about(1.0, factor(dy, h), 0.0, b.top),
        Handle::Top => about(1.0, factor(-dy, h), 0.0, b.bottom),
        Handle::BottomRight => {
            let s = diagonal(dx, dy);
            about(s, s, b.left, b.top)
        }
        Handle::TopRight => {
            let s = diagonal(dx, -dy);
            about(s, s, b.left, b.bottom)
        }
        Handle::BottomLeft => {
            let s = diagonal(-dx, dy);
            about(s, s, b.right, b.top)
        }
        Handle::TopLeft => {
            let s = diagonal(-dx, -dy);
            about(s, s, b.right, b.bottom)
        }
    }
}

/// A box under a map, normalised. Exact for the axis-aligned maps a drag makes.
fn mapped(r: accent_core::pdf::Rect, m: accent_core::pdf::Matrix) -> accent_core::pdf::Rect {
    let (a, b) = (
        accent_core::pdf::apply(m, (r.left, r.top)),
        accent_core::pdf::apply(m, (r.right, r.bottom)),
    );
    accent_core::pdf::Rect {
        left: a.0.min(b.0),
        top: a.1.min(b.1),
        right: a.0.max(b.0),
        bottom: a.1.max(b.1),
    }
}

/// The stroke the Adjust tool has hold of, and the drag being applied to it.
pub struct Selected {
    pub page: usize,
    /// Its place in the page's `/Annots` as of the last [`Reply::Inks`].
    pub index: usize,
    pub points: Vec<(f32, f32)>,
    pub bounds: accent_core::pdf::Rect,
    pub style: accent_core::pdf::InkStyle,
    /// Where the hand is holding it, while it is.
    pub handle: Option<Handle>,
    /// What the drag so far amounts to, painted over the page until the hand lets go.
    pub matrix: accent_core::pdf::Matrix,
}

/// A stroke as the widget holds it while it is being drawn.
pub struct Stroke {
    pub page: usize,
    /// The points, in that page's own points.
    pub points: Vec<(f32, f32)>,
    /// The tool that drew it, which is what the overlay is painted like.
    pub tool: Mode,
    /// Whether the hand has let go. A finished stroke stays painted until a tile carries it.
    pub done: bool,
}

/// Where every note link that highlights a document lands, per page: the quads to paint and the
/// index of the link each came from.
pub type Highlights = HashMap<usize, Vec<(Vec<accent_core::pdf::Rect>, usize)>>;

/// What the render thread sends back. Every variant is `Send`, because each one travels to the
/// main loop inside its own idle callback.
pub enum Reply {
    Tile(TileKey, accent_core::pdf::RgbaImage),
    Lowres {
        page: u32,
        dark: bool,
        image: accent_core::pdf::RgbaImage,
    },
    Links(usize, Vec<accent_core::pdf::Link>),
    /// One page's glyphs and their boxes, for selecting text on it.
    Text(usize, Vec<accent_core::pdf::Glyph>),
    Outline(Vec<accent_core::pdf::Outline>),
    /// One page's matches for the query identified by `query`; a later one abandons it.
    Found {
        query: u64,
        page: usize,
        hits: Vec<Vec<accent_core::pdf::Rect>>,
    },
    /// The file was read: these are its page sizes. The first one arrives when the document is
    /// opened, which is why a tab can be on screen before anything is known about it.
    Reloaded(Vec<(f32, f32)>),
    /// Where every note link that highlights this document lands on the page today, and which
    /// link each one is. The whole map every time, so a stale page cannot survive underneath.
    Highlights(Highlights),
    /// An export finished: how many annotations it wrote, or why it could not.
    Exported(Result<usize, String>),
    /// Every ink stroke of one page with its box and style, for the Adjust tool to take hold of.
    Inks {
        page: usize,
        inks: Vec<accent_core::pdf::InkShape>,
    },
    /// This page's annotations changed, so what is cached of it is of the old page.
    PageChanged(usize),
    /// The file now on disk is ours, and this is its etag — which is how the tab tells its own
    /// write from someone else's and does not reload over strokes drawn since.
    Saved(accent_core::fs::Etag),
    /// The document could not be opened at all, with the reason to show in its place.
    Failed(String),
}

/// Lay the pages out in one column at `scale`, centred in `viewport_w`.
///
/// Pure, so the arithmetic that decides what is on screen is testable without a display.
pub fn layout(sizes: &[(f32, f32)], scale: f32, viewport_w: f32) -> Layout {
    let widest = sizes.iter().map(|(w, _)| w * scale).fold(0.0, f32::max);
    let width = (widest + GAP * 2.0).max(viewport_w);
    let mut pages = Vec::with_capacity(sizes.len());
    let mut y = GAP;
    for (w, h) in sizes {
        let (w, h) = (w * scale, h * scale);
        pages.push(PageRect {
            x: ((width - w) / 2.0).max(GAP),
            y,
            w,
            h,
        });
        y += h + GAP;
    }
    Layout {
        scale,
        pages,
        width,
        height: y.max(1.0),
    }
}

/// The scale a zoom mode asks for, given the viewport it has to fit into.
pub fn fit_scale(sizes: &[(f32, f32)], zoom: PdfZoom, vw: f32, vh: f32) -> f32 {
    let widest = sizes.iter().map(|(w, _)| *w).fold(1.0, f32::max);
    let tallest = sizes.iter().map(|(_, h)| *h).fold(1.0, f32::max);
    let width = ((vw - GAP * 2.0).max(1.0)) / widest;
    match zoom {
        PdfZoom::FitWidth => width,
        // The smaller of the two, so the whole page really is on screen.
        PdfZoom::FitPage => width.min(((vh - GAP * 2.0).max(1.0)) / tallest),
        PdfZoom::Scale(z) => z as f32 * PT_TO_PX,
    }
}

/// One zoom step in or out from the zoom `from`. The arithmetic is the document's, so a page
/// steps in the same tenths a note does however far a fit mode left it from one.
///
/// `from` is a zoom, not a layout scale: reading the step back out of [`Layout::scale`] means
/// dividing an `f32` by [`PT_TO_PX`], and past 230 % the drift that leaves is larger than
/// [`crate::stepped_zoom`]'s epsilon, so the next tenth is the one the page is already at and
/// the zoom stops moving.
pub fn stepped(from: f64, out: bool) -> PdfZoom {
    PdfZoom::Scale(crate::stepped_zoom(from, out).clamp(MIN_SCALE, MAX_SCALE))
}

/// Where a reading position resumes from, given the zoom it resumes into.
///
/// Under [`PdfZoom::FitPage`] one page is one screen, so the page lands at its top. Keeping the
/// fraction of it the viewport happened to be left at is what made Fit Page look broken: the
/// scale was right, but the same half of one page and half of the next stayed on screen.
pub fn resume_at(zoom: PdfZoom, anchor: Anchor) -> Anchor {
    match zoom {
        PdfZoom::FitPage => Anchor {
            u: 0.0,
            v: 0.0,
            ..anchor
        },
        _ => anchor,
    }
}

/// A scale clamped to what is worth rendering: below the floor nothing is legible, above the
/// ceiling one page is hundreds of megabytes of tiles.
pub fn clamp_scale(scale: f32) -> f32 {
    scale.clamp((MIN_SCALE as f32) * PT_TO_PX, (MAX_SCALE as f32) * PT_TO_PX)
}

/// The content offset `(x, y)` as a reading position: which page the top-left of the viewport is
/// in, and how far into it.
pub fn anchor_at(layout: &Layout, x: f64, y: f64) -> Anchor {
    // A pixel down, so an offset resting exactly on a page's top edge is that page rather than
    // the gap above it.
    let page = page_at(layout, y + 1.0);
    match layout.pages.get(page) {
        Some(rect) => Anchor {
            page,
            u: ((x - f64::from(rect.x)) / f64::from(rect.w.max(1.0))) as f32,
            v: ((y - f64::from(rect.y)) / f64::from(rect.h.max(1.0))) as f32,
        },
        None => Anchor::default(),
    }
}

/// Where a reading position sits in the content, or `None` for a page this layout has not got.
pub fn offset_of(layout: &Layout, anchor: Anchor) -> Option<(f64, f64)> {
    let rect = layout.pages.get(anchor.page)?;
    Some((
        f64::from(rect.x + anchor.u * rect.w),
        f64::from(rect.y + anchor.v * rect.h),
    ))
}

/// Which page a content coordinate falls in, or the nearest one above it.
fn page_at(layout: &Layout, y: f64) -> usize {
    layout
        .pages
        .iter()
        .rposition(|rect| f64::from(rect.y) <= y)
        .unwrap_or(0)
}

glib::wrapper! {
    pub struct PdfView(ObjectSubclass<imp::PdfView>)
        @extends gtk::Widget,
        @implements gtk::Scrollable, gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for PdfView {
    fn default() -> Self {
        Self::new()
    }
}

impl PdfView {
    pub fn new() -> PdfView {
        glib::Object::new()
    }

    /// A narrow view of the same document: low-resolution pages only, with the one being read
    /// framed, and a click to jump to a page.
    pub fn thumbnails() -> PdfView {
        let view: PdfView = glib::Object::new();
        view.imp().thumbnails.set(true);
        view.imp().zoom.set(PdfZoom::FitWidth);
        view
    }

    /// The page sizes in points, which is everything the layout needs to know about the document.
    pub fn set_sizes(&self, sizes: Vec<(f32, f32)>) {
        *self.imp().sizes.borrow_mut() = sizes;
        self.relayout();
    }

    pub fn page_count(&self) -> usize {
        self.imp().sizes.borrow().len()
    }

    /// The cache, shared with every view of the same document.
    pub fn set_cache(&self, cache: std::rc::Rc<RefCell<Cache>>) {
        let _ = self.imp().cache.set(cache);
    }

    pub fn cache(&self) -> std::rc::Rc<RefCell<Cache>> {
        self.imp()
            .cache
            .get_or_init(|| std::rc::Rc::new(RefCell::new(Cache::default())))
            .clone()
    }

    pub fn zoom(&self) -> PdfZoom {
        self.imp().zoom.get()
    }

    pub fn set_zoom(&self, zoom: PdfZoom) {
        if self.imp().zoom.get() == zoom {
            return;
        }
        self.imp().zoom.set(zoom);
        self.relayout();
        self.zoomed();
    }

    /// Zoom one step, keeping whatever is under `at` (a widget coordinate) where it is.
    pub fn zoom_step(&self, out: bool, at: Option<(f64, f64)>) {
        let from = match self.imp().zoom.get() {
            // Exact, so a step never has to be read back out of the laid-out `f32` scale.
            PdfZoom::Scale(zoom) => zoom,
            // A fit mode has no percentage of its own: step from wherever it left the page.
            _ => f64::from(self.imp().layout.borrow().scale) / f64::from(PT_TO_PX),
        };
        self.zoom_around(stepped(from, out), at);
    }

    fn zoom_around(&self, zoom: PdfZoom, at: Option<(f64, f64)>) {
        let Some((x, y)) = at else {
            return self.set_zoom(zoom);
        };
        let (before_x, before_y) = self.content_at(x, y);
        let (old_w, old_h) = {
            let layout = self.imp().layout.borrow();
            (layout.width, layout.height)
        };
        self.imp().zoom.set(zoom);
        self.relayout();
        let layout = self.imp().layout.borrow().clone();
        // The same fraction of the content stays under the pointer, which is what makes zooming
        // feel like moving the page rather than moving the window.
        let (fx, fy) = (before_x / old_w.max(1.0), before_y / old_h.max(1.0));
        let (hadj, vadj) = (self.hadjustment(), self.vadjustment());
        if let Some(hadj) = hadj {
            hadj.set_value(f64::from(fx * layout.width) - x);
        }
        if let Some(vadj) = vadj {
            vadj.set_value(f64::from(fy * layout.height) - y);
        }
        self.zoomed();
    }

    /// Called whenever the zoom changed, however it changed: a chord, the wheel or a pinch.
    /// [`PdfView::set_zoom`] and [`PdfView::zoom_around`] are the only writers there are.
    pub fn connect_zoom(&self, f: impl Fn() + 'static) {
        *self.imp().on_zoom.borrow_mut() = Some(Box::new(f));
    }

    fn zoomed(&self) {
        let handler = self.imp().on_zoom.borrow();
        if let Some(f) = handler.as_ref() {
            f();
        }
    }

    /// Paint for a dark theme. The rendering itself inverts, so this changes which tiles are
    /// wanted rather than how they are drawn.
    pub fn set_dark(&self, dark: bool) {
        if self.imp().dark.replace(dark) != dark {
            self.queue_draw();
        }
    }

    pub fn dark(&self) -> bool {
        self.imp().dark.get()
    }

    /// Take a rendered tile. Ignored if the document has moved on from the scale it was for.
    pub fn insert_tile(&self, key: TileKey, texture: gdk::MemoryTexture, bytes: usize) {
        self.imp().stale_tiles.borrow_mut().remove(&key);
        self.cache().borrow_mut().insert(key, texture, bytes);
        // Whatever was drawn on this page is in its pixels now, so the stroke painted over the
        // top can go. Here rather than when the stroke was sent: the render is what replaces it,
        // and dropping it any earlier is a gap the reader sees.
        self.settle_stroke(key.page as usize);
        self.queue_draw();
    }

    pub fn insert_lowres(&self, page: u32, dark: bool, texture: gdk::MemoryTexture) {
        self.cache().borrow_mut().insert_lowres(page, dark, texture);
        self.queue_draw();
    }

    /// Called with the tiles this view wants and does not have, visible ones first.
    pub fn connect_wants(&self, f: impl Fn(&PdfView, f32, bool, Vec<Want>) + 'static) {
        *self.imp().on_wants.borrow_mut() = Some(Box::new(f));
    }

    /// Called with everything from the render thread that is not a texture.
    pub fn connect_reply(&self, f: impl Fn(&PdfView, Reply) + 'static) {
        *self.imp().on_reply.borrow_mut() = Some(Box::new(f));
    }

    /// Take one answer from the render thread. Textures go straight into the shared cache; the
    /// rest is the tab's business and goes out through [`PdfView::connect_reply`].
    pub fn deliver(&self, reply: Reply) {
        match reply {
            Reply::Tile(key, image) => {
                let bytes = image.data.len();
                self.insert_tile(key, texture(image), bytes);
            }
            Reply::Lowres { page, dark, image } => {
                self.insert_lowres(page, dark, texture(image));
                let handler = self.imp().on_lowres.borrow();
                if let Some(f) = handler.as_ref() {
                    f(page);
                }
            }
            other => {
                let handler = self.imp().on_reply.borrow();
                if let Some(f) = handler.as_ref() {
                    f(self, other);
                }
            }
        }
    }

    /// Forget every texture: the file changed, so all of them are of the old one.
    pub fn forget_textures(&self) {
        self.cache().borrow_mut().clear();
        self.imp().asked.borrow_mut().clear();
        self.queue_draw();
    }

    /// Called with a page number when a click or a key asks to go somewhere.
    pub fn connect_goto(&self, f: impl Fn(usize) + 'static) {
        *self.imp().on_goto.borrow_mut() = Some(Box::new(f));
    }

    /// Called with a widget coordinate on every primary click, for link hit-testing.
    pub fn connect_pressed(&self, f: impl Fn(&PdfView, f64, f64) + 'static) {
        *self.imp().on_pressed.borrow_mut() = Some(Box::new(f));
    }

    /// Called when a press turns out to have been a click rather than the start of a drag.
    ///
    /// A highlight opens the note that holds it, and that must not fire on every drag that
    /// happens to begin inside one — so it waits for the release, unlike the link handler above,
    /// which answers on the press because following a link is what a press on one means.
    pub fn connect_clicked(&self, f: impl Fn(&PdfView, f64, f64) + 'static) {
        *self.imp().on_clicked.borrow_mut() = Some(Box::new(f));
    }

    /// Called on pointer motion, so the tab can show a hand over a link.
    pub fn connect_motion(&self, f: impl Fn(&PdfView, f64, f64) + 'static) {
        *self.imp().on_motion.borrow_mut() = Some(Box::new(f));
    }

    /// Called with a page number when its low-resolution stand-in lands in the cache. The link
    /// preview waits on this: the page it wants a band of has usually never been drawn.
    pub fn connect_lowres(&self, f: impl Fn(u32) + 'static) {
        *self.imp().on_lowres.borrow_mut() = Some(Box::new(f));
    }

    /// One page's size in points, as the document reported it.
    pub fn page_size(&self, page: usize) -> Option<(f32, f32)> {
        self.imp().sizes.borrow().get(page).copied()
    }

    /// Called with the page under the middle of the viewport whenever it changes.
    pub fn connect_page(&self, f: impl Fn(usize) + 'static) {
        *self.imp().on_page.borrow_mut() = Some(Box::new(f));
    }

    /// Rectangles to paint over the page, in page points, per page: the search matches.
    pub fn set_marks(&self, marks: HashMap<usize, Vec<accent_core::pdf::Rect>>) {
        *self.imp().marks.borrow_mut() = marks;
        self.queue_draw();
    }

    /// Where the note links that highlight this document land, per page, each with the index of
    /// the link it came from.
    pub fn set_highlights(&self, highlights: Highlights) {
        *self.imp().highlights.borrow_mut() = highlights;
        self.queue_draw();
    }

    /// Which highlight is under this widget coordinate, if any.
    pub fn highlight_at(&self, x: f64, y: f64) -> Option<usize> {
        let (page, px, py) = self.page_point(x, y)?;
        let highlights = self.imp().highlights.borrow();
        highlights.get(&page)?.iter().find_map(|(quads, link)| {
            let inside = quads
                .iter()
                .any(|q| (q.left..=q.right).contains(&px) && (q.top..=q.bottom).contains(&py));
            inside.then_some(*link)
        })
    }

    /// What a drag over the page does.
    pub fn mode(&self) -> Mode {
        self.imp().mode.get()
    }

    pub fn set_mode(&self, mode: Mode) {
        self.imp().mode.set(mode);
        if mode != Mode::Adjust {
            self.clear_inks();
        }
        // The plain pointer for all three, and never the I-beam the page otherwise shows: with a
        // tool in hand a drag draws rather than selects, and a cursor that says "text" invites
        // exactly the thing that will not happen.
        self.set_cursor_from_name(match mode {
            Mode::Select => None,
            _ => Some("default"),
        });
    }

    /// What the preferences say about the tools.
    pub fn set_drawing_config(&self, config: accent_core::config::DrawingConfig) {
        *self.imp().style.borrow_mut() = config;
        self.queue_draw();
    }

    pub fn drawing_config(&self) -> accent_core::config::DrawingConfig {
        self.imp().style.borrow().clone()
    }

    /// How a tool's stroke is drawn, from the preferences: the highlighter in its own width and
    /// colour, wide and translucent; everything else — the pen and the shapes — in the pen's.
    /// One width per stroke: a stylus reports pressure and this ignores it.
    ///
    // ponytail: uniform width because varying it means storing a width per point and drawing
    // the stroke as a filled outline rather than a stroked path. A `GestureStylus` reading
    // pressure is the upgrade.
    pub fn ink_style(&self, mode: Mode) -> accent_core::pdf::InkStyle {
        let c = self.imp().style.borrow();
        let (width, colour, alpha, multiply) = match mode {
            Mode::Highlighter => (
                c.highlighter_width,
                c.highlighter_color,
                HIGHLIGHTER_ALPHA,
                true,
            ),
            _ => (c.pen_width, c.pen_color, 1.0, false),
        };
        let [r, g, b] = colour.unwrap_or_else(crate::theme::accent_rgb);
        accent_core::pdf::InkStyle {
            width,
            rgba: [r, g, b, (alpha * 255.0) as u8],
            multiply,
        }
    }

    /// The tool a press really means. The mode itself while nothing but a mouse is plugged in;
    /// with a pen attached the hand selects and only the pen draws, unless the preference says
    /// otherwise, and the pen's eraser tip erases whatever is in hand.
    fn effective_mode(&self, controller: &impl IsA<gtk::EventController>) -> Mode {
        let mode = self.imp().mode.get();
        if mode == Mode::Select {
            return mode;
        }
        let tool = controller.current_event().and_then(|e| e.device_tool());
        if tool.is_some_and(|t| t.tool_type() == gdk::DeviceToolType::Eraser) {
            return Mode::Eraser;
        }
        let pen = controller
            .current_event_device()
            .is_some_and(|d| d.source() == gdk::InputSource::Pen);
        match !pen && !self.imp().style.borrow().mouse && stylus_attached() {
            true => Mode::Select,
            false => mode,
        }
    }

    /// Called with a finished stroke: the page and its points, in that page's own points.
    pub fn connect_ink(&self, f: impl Fn(usize, Vec<(f32, f32)>) + 'static) {
        *self.imp().on_ink.borrow_mut() = Some(Box::new(f));
    }

    /// Called with a point the eraser passed over.
    pub fn connect_erase(&self, f: impl Fn(usize, (f32, f32)) + 'static) {
        *self.imp().on_erase.borrow_mut() = Some(Box::new(f));
    }

    /// Called with a page, the index of a stroke on it and the map to move it by.
    pub fn connect_transform(&self, f: impl Fn(usize, usize, accent_core::pdf::Matrix) + 'static) {
        *self.imp().on_transform.borrow_mut() = Some(Box::new(f));
    }

    /// What one page holds for the Adjust tool. A selection on that page follows its stroke
    /// to the fresh list, or to the list's end after a move, or goes if the stroke did.
    pub fn set_inks(&self, page: usize, inks: Vec<accent_core::pdf::InkShape>) {
        {
            let mut adjust = self.imp().adjust.borrow_mut();
            if let Some(a) = adjust.as_ref()
                && a.page == page
            {
                let fresh = match self.imp().reselect.replace(false) {
                    true => inks.last(),
                    false => inks.iter().find(|i| i.index == a.index),
                };
                *adjust = fresh.map(|i| Selected {
                    page,
                    index: i.index,
                    points: i.points.clone(),
                    bounds: i.bounds,
                    style: i.style,
                    handle: a.handle,
                    matrix: a.matrix,
                });
            }
        }
        self.imp().inks.borrow_mut().insert(page, inks);
        self.queue_draw();
    }

    /// Forget every stroke the Adjust tool knew: the document changed under it.
    pub fn clear_inks(&self) {
        self.imp().inks.borrow_mut().clear();
        *self.imp().adjust.borrow_mut() = None;
        self.queue_draw();
    }

    /// The Adjust tool's press: a handle of the stroke already held, else whichever stroke is
    /// under the pointer, else nothing. Whether the press took hold of anything.
    fn adjust_press(&self, x: f64, y: f64) -> bool {
        let Some((page, at)) = self.nearest_page_point(x, y) else {
            return false;
        };
        let grip = HANDLE / self.imp().layout.borrow().scale;
        let mut adjust = self.imp().adjust.borrow_mut();
        if let Some(a) = adjust.as_mut()
            && a.page == page
            && let Some(handle) = handle_at(a.bounds, at, grip)
        {
            a.handle = Some(handle);
            return true;
        }
        // The stroke under the pointer first, then the first whose box it is inside: a thin
        // line wants the pointer on it, a circle's empty middle still belongs to the circle.
        let inks = self.imp().inks.borrow();
        let found = inks.get(&page).and_then(|inks| {
            inks.iter()
                .find(|i| accent_core::pdf::hit(&i.points, at, ADJUST_RADIUS))
                .or_else(|| {
                    inks.iter()
                        .find(|i| handle_at(i.bounds, at, grip).is_some())
                })
        });
        *adjust = found.map(|i| Selected {
            page,
            index: i.index,
            points: i.points.clone(),
            bounds: i.bounds,
            style: i.style,
            handle: Some(Handle::Move),
            matrix: accent_core::pdf::IDENTITY,
        });
        let held = adjust.is_some();
        drop(adjust);
        self.queue_draw();
        held
    }

    fn adjust_drag(&self, dx: f64, dy: f64) {
        let scale = self.imp().layout.borrow().scale;
        if let Some(a) = self.imp().adjust.borrow_mut().as_mut()
            && let Some(handle) = a.handle
        {
            a.matrix = drag_matrix(handle, a.bounds, dx as f32 / scale, dy as f32 / scale);
        }
        self.queue_draw();
    }

    /// The hand let go: what the drag amounted to goes to the tab, and the selection keeps
    /// painting where the stroke will be until the fresh list confirms it.
    fn adjust_release(&self) {
        let sent = {
            let mut adjust = self.imp().adjust.borrow_mut();
            let Some(a) = adjust.as_mut() else {
                return;
            };
            a.handle = None;
            let m = std::mem::replace(&mut a.matrix, accent_core::pdf::IDENTITY);
            if m == accent_core::pdf::IDENTITY {
                return;
            }
            a.points = a
                .points
                .iter()
                .map(|&p| accent_core::pdf::apply(m, p))
                .collect();
            a.bounds = mapped(a.bounds, m);
            (a.page, a.index, m)
        };
        self.imp().reselect.set(true);
        if let Some(f) = self.imp().on_transform.borrow().as_ref() {
            f(sent.0, sent.1, sent.2);
        }
        self.queue_draw();
    }

    /// Tell the tab the eraser passed over this point of whichever page is under it.
    fn erase_at(&self, x: f64, y: f64) {
        let Some((page, _)) = self.nearest_page_point(x, y) else {
            return;
        };
        let at = self.point_on(page, x, y);
        if let Some(f) = self.imp().on_erase.borrow().as_ref() {
            f(page, at);
        }
    }

    /// Drop the finished strokes of a page that is now rendered with them.
    pub fn settle_stroke(&self, page: usize) {
        let mut strokes = self.imp().strokes.borrow_mut();
        let before = strokes.len();
        strokes.retain(|stroke| !(stroke.done && stroke.page == page));
        if strokes.len() != before {
            drop(strokes);
            self.queue_draw();
        }
    }

    /// A point on a page in that page's own points, clamped into the paper.
    ///
    /// The clamp is the page edge: a stroke pulled off the paper stops at it rather than being
    /// drawn where no viewer would show it.
    fn point_on(&self, page: usize, x: f64, y: f64) -> (f32, f32) {
        let (cx, cy) = self.content_at(x, y);
        let layout = self.imp().layout.borrow();
        let (size, rect) = (self.page_size(page), layout.pages.get(page).copied());
        let Some((rect, (pw, ph))) = rect.zip(size) else {
            return (0.0, 0.0);
        };
        (
            ((cx - rect.x) / layout.scale).clamp(0.0, pw),
            ((cy - rect.y) / layout.scale).clamp(0.0, ph),
        )
    }

    /// Draw one page again, because what it holds changed — a stroke, an erase, an export.
    ///
    /// Deliberately not an eviction. What is on screen keeps being painted until its replacement
    /// arrives, so a stroke costs one re-render and no blank page in between.
    pub fn refresh_page(&self, page: usize) {
        let (scale_milli, dark) = self.stamp();
        self.cache()
            .borrow_mut()
            .forget_page_except(page as u32, scale_milli, dark);
        self.imp().stale_pages.borrow_mut().insert(page as u32);
        self.queue_draw();
    }

    /// The scale and colour scheme the page is being painted at, which is what a cached tile is
    /// keyed by. One definition, shared by the snapshot and by [`PdfView::refresh_page`].
    fn stamp(&self) -> (u32, bool) {
        let scale = self.imp().layout.borrow().scale * self.scale_factor().max(1) as f32;
        ((scale * 1000.0).round() as u32, self.imp().dark.get())
    }

    /// The one match to draw more strongly than the rest.
    pub fn set_current_mark(&self, at: Option<(usize, usize)>) {
        self.imp().current_mark.set(at);
        self.queue_draw();
    }

    /// The boxes of the selected glyphs, in page points, per page: one entry for a selection
    /// inside a page, one per page for a drag that ran across a page break.
    pub fn set_selection(&self, selection: Vec<(usize, Vec<accent_core::pdf::Rect>)>) {
        *self.imp().selection.borrow_mut() = selection;
        self.queue_draw();
    }

    /// Called with the page and the point a drag started and ended on, which need not be the
    /// same page.
    pub fn connect_select(&self, f: impl Fn(&PdfView, Span) + 'static) {
        *self.imp().on_select.borrow_mut() = Some(Box::new(f));
    }

    /// Where the reader is now.
    pub fn anchor(&self) -> Anchor {
        let (x, y) = self.scroll_offset();
        anchor_at(&self.imp().layout.borrow(), x, y)
    }

    /// The layout as one line: what `ACCENT_BENCH_PDF` prints, and its only reader. Whether the
    /// page being read is wholly on screen is what Fit Page has to mean, so that is the last
    /// field rather than something the numbers have to be read for.
    pub fn geometry(&self) -> String {
        let layout = self.imp().layout.borrow();
        let (_, y) = self.scroll_offset();
        let vh = f64::from(self.height());
        let page = self.current_page();
        let rect = layout.pages.get(page).copied();
        let whole = rect.is_some_and(|r| f64::from(r.y) >= y && f64::from(r.y + r.h) <= y + vh);
        let (page_y, page_h) = rect.map_or((0.0, 0.0), |r| (r.y, r.h));
        format!(
            "zoom={:?} scale={:.4} vw={} vh={vh} content_h={:.1} top={y:.1} page={page} \
             page_y={page_y:.1} page_h={page_h:.1} whole_page={whole}",
            self.zoom(),
            layout.scale,
            self.width(),
            layout.height,
        )
    }

    /// Put the reader back where `anchor` says, or at the top of its page under
    /// [`PdfZoom::FitPage`]. See [`resume_at`].
    pub fn scroll_to(&self, anchor: Anchor) {
        let anchor = resume_at(self.imp().zoom.get(), anchor);
        let at = offset_of(&self.imp().layout.borrow(), anchor);
        let Some((x, y)) = at else {
            return;
        };
        if let Some(hadj) = self.hadjustment() {
            hadj.set_value(x);
        }
        if let Some(vadj) = self.vadjustment() {
            vadj.set_value(y);
        }
    }

    /// Scroll so `page` starts at the top of the viewport, `top` points down it if given.
    pub fn goto_page(&self, page: usize, top: Option<f32>) {
        let scale = self.imp().layout.borrow().scale;
        let v = top.map_or(0.0, |top| top * scale);
        let layout = self.imp().layout.borrow().clone();
        let Some(rect) = layout.pages.get(page) else {
            return;
        };
        if let Some(vadj) = self.vadjustment() {
            vadj.set_value(f64::from(rect.y + v));
        }
    }

    /// Scroll one step up or down, which is what the arrow keys ask for.
    ///
    /// `GtkScrolledWindow` binds a step to `Ctrl+Up` and `Ctrl+Down` and the bare arrows to
    /// nothing, so this is the same move under the key a reader reaches for. The adjustment
    /// clamps its own value, so the two ends of the document need no case here.
    pub fn scroll_step(&self, down: bool) {
        let Some(vadj) = self.vadjustment() else {
            return;
        };
        let step = vadj.step_increment();
        vadj.set_value(vadj.value() + if down { step } else { -step });
    }

    /// Bring a rectangle of a page into view, for a search match.
    pub fn reveal(&self, page: usize, rect: accent_core::pdf::Rect) {
        let layout = self.imp().layout.borrow().clone();
        let Some(page_rect) = layout.pages.get(page) else {
            return;
        };
        let Some(vadj) = self.vadjustment() else {
            return;
        };
        let top = f64::from(page_rect.y + rect.top * layout.scale);
        let bottom = f64::from(page_rect.y + rect.bottom * layout.scale);
        let (value, size) = (vadj.value(), vadj.page_size());
        // Only if it is not already comfortably on screen, so stepping through matches on one
        // page does not jerk the view for each of them.
        if top < value || bottom > value + size {
            vadj.set_value(top - size / 3.0);
        }
    }

    /// The page under the middle of the viewport: what "page 4 of 12" means.
    pub fn current_page(&self) -> usize {
        let (_, y) = self.scroll_offset();
        let middle = y + self.vadjustment().map_or(0.0, |a| a.page_size()) / 2.0;
        page_at(&self.imp().layout.borrow(), middle)
    }

    /// Turn a widget coordinate into a page and a point on it.
    pub fn page_point(&self, x: f64, y: f64) -> Option<(usize, f32, f32)> {
        let (cx, cy) = self.content_at(x, y);
        let layout = self.imp().layout.borrow();
        let (page, rect) = layout
            .pages
            .iter()
            .enumerate()
            .find(|(_, r)| cy >= r.y && cy <= r.y + r.h && cx >= r.x && cx <= r.x + r.w)?;
        Some((
            page,
            (cx - rect.x) / layout.scale,
            (cy - rect.y) / layout.scale,
        ))
    }

    /// Report the two ends of a drag, each as a page and a point on it.
    fn select_between(&self, x0: f64, y0: f64, x1: f64, y1: f64) {
        let (Some(from), Some(to)) = (
            self.nearest_page_point(x0, y0),
            self.nearest_page_point(x1, y1),
        ) else {
            return;
        };
        let handler = self.imp().on_select.borrow();
        if let Some(f) = handler.as_ref() {
            f(self, Span { from, to });
        }
    }

    /// Like [`PdfView::page_point`], but for a point that is off the paper: the gap between two
    /// pages, or the margin beside one.
    ///
    /// A drag has to keep working there, and it does not need clamping to do so — the page's own
    /// coordinates simply run negative or past its height, and picking the glyph nearest such a
    /// point is what a drag off the bottom of a page means anyway.
    fn nearest_page_point(&self, x: f64, y: f64) -> Option<(usize, (f32, f32))> {
        let (cx, cy) = self.content_at(x, y);
        let layout = self.imp().layout.borrow();
        let page = page_at(&layout, f64::from(cy));
        let rect = layout.pages.get(page)?;
        Some((
            page,
            ((cx - rect.x) / layout.scale, (cy - rect.y) / layout.scale),
        ))
    }

    fn content_at(&self, x: f64, y: f64) -> (f32, f32) {
        let (ox, oy) = self.scroll_offset();
        ((x + ox) as f32, (y + oy) as f32)
    }

    fn scroll_offset(&self) -> (f64, f64) {
        (
            self.hadjustment().map_or(0.0, |a| a.value()),
            self.vadjustment().map_or(0.0, |a| a.value()),
        )
    }

    fn hadjustment(&self) -> Option<gtk::Adjustment> {
        self.imp().hadjustment.borrow().clone()
    }

    fn vadjustment(&self) -> Option<gtk::Adjustment> {
        self.imp().vadjustment.borrow().clone()
    }

    /// Recompute the layout for the current size and zoom, and tell the scrollbars.
    ///
    /// The reading position is kept across the recompute, because it is the one thing the raw
    /// scroll offset cannot carry: a resize or a zoom moves every page, so the same number of
    /// pixels down the content is a different place in the document.
    fn relayout(&self) {
        let (w, h) = (self.width(), self.height());
        if w <= 1 || h <= 1 {
            return;
        }
        let sizes = self.imp().sizes.borrow().clone();
        if sizes.is_empty() {
            return;
        }
        // Nothing to keep before the first layout: the offset is zero and page one is where the
        // reader is anyway.
        let anchor = (!self.imp().layout.borrow().pages.is_empty()).then(|| self.anchor());
        let scale = clamp_scale(fit_scale(&sizes, self.imp().zoom.get(), w as f32, h as f32));
        let layout = layout(&sizes, scale, w as f32);
        let (width, height) = (f64::from(layout.width), f64::from(layout.height));
        *self.imp().layout.borrow_mut() = layout;
        configure(self.hadjustment(), width, f64::from(w));
        configure(self.vadjustment(), height, f64::from(h));
        if let Some(anchor) = anchor {
            self.scroll_to(anchor);
        }
        self.queue_draw();
    }
}

/// Point an adjustment at a content size without disturbing where it is scrolled to.
///
/// The upper bound is never below the page size, which GTK asserts on and which a document
/// smaller than the window otherwise breaks — an A4 sketch in a split pane, or any small page in
/// a large one. There is nothing to scroll in that case either way: the value clamps to zero.
/// Whether the seat has a stylus at all. Asked on the press rather than cached: a tablet can be
/// plugged in mid-session.
fn stylus_attached() -> bool {
    gdk::Display::default()
        .and_then(|d| d.default_seat())
        .is_some_and(|s| !s.devices(gdk::SeatCapabilities::TABLET_STYLUS).is_empty())
}

fn configure(adjustment: Option<gtk::Adjustment>, upper: f64, page: f64) {
    let Some(adjustment) = adjustment else {
        return;
    };
    let value = adjustment.value().min((upper - page).max(0.0));
    adjustment.configure(value, 0.0, upper.max(page), page * 0.1, page * 0.9, page);
}

mod imp {
    use super::*;
    use std::cell::OnceCell;

    type Wants = Box<dyn Fn(&super::PdfView, f32, bool, Vec<Want>)>;
    type Coords = Box<dyn Fn(&super::PdfView, f64, f64)>;
    type Page = Box<dyn Fn(usize)>;
    type Zoomed = Box<dyn Fn()>;
    type OnReply = Box<dyn Fn(&super::PdfView, Reply)>;
    type OnSelect = Box<dyn Fn(&super::PdfView, super::Span)>;
    type Lowres = Box<dyn Fn(u32)>;
    type Stroke = Box<dyn Fn(usize, Vec<(f32, f32)>)>;
    type At = Box<dyn Fn(usize, (f32, f32))>;
    type Transform = Box<dyn Fn(usize, usize, accent_core::pdf::Matrix)>;

    #[derive(glib::Properties)]
    #[properties(wrapper_type = super::PdfView)]
    pub struct PdfView {
        // The four `GtkScrollable` properties. GTK reads and writes them by name, so they have to
        // be real GObject properties rather than plain fields.
        #[property(get, set = Self::adopt_h, nullable, override_interface = gtk::Scrollable)]
        pub hadjustment: RefCell<Option<gtk::Adjustment>>,
        #[property(get, set = Self::adopt_v, nullable, override_interface = gtk::Scrollable)]
        pub vadjustment: RefCell<Option<gtk::Adjustment>>,
        #[property(get, set, override_interface = gtk::Scrollable, builder(gtk::ScrollablePolicy::Minimum))]
        pub hscroll_policy: Cell<gtk::ScrollablePolicy>,
        #[property(get, set, override_interface = gtk::Scrollable, builder(gtk::ScrollablePolicy::Minimum))]
        pub vscroll_policy: Cell<gtk::ScrollablePolicy>,
        /// The handlers on the two adjustments, dropped when they are replaced.
        pub adj_handlers: RefCell<[Option<glib::SignalHandlerId>; 2]>,
        pub sizes: RefCell<Vec<(f32, f32)>>,
        pub layout: RefCell<super::Layout>,
        pub cache: OnceCell<std::rc::Rc<RefCell<Cache>>>,
        pub zoom: Cell<PdfZoom>,
        pub dark: Cell<bool>,
        pub thumbnails: Cell<bool>,
        pub marks: RefCell<HashMap<usize, Vec<accent_core::pdf::Rect>>>,
        /// Where the note links that highlight this document land, per page, each with the index
        /// of the link it came from so a click on one can open the note that holds it.
        pub highlights: RefCell<super::Highlights>,
        /// The selected glyphs' boxes, per page the selection covers.
        pub selection: RefCell<Vec<(usize, Vec<accent_core::pdf::Rect>)>>,
        /// Where a drag began, in widget coordinates, while one is in progress.
        pub drag_from: Cell<Option<(f64, f64)>>,
        /// What a drag over the page does: select, draw, or erase.
        pub mode: Cell<super::Mode>,
        /// The tool this drag really uses — the mode, or Select for a mouse press while a pen
        /// is attached, or Eraser for the stylus's eraser tip — decided once, on the press.
        pub drag_mode: Cell<super::Mode>,
        /// What the config says about the tools; see [`super::PdfView::set_drawing_config`].
        pub style: RefCell<accent_core::config::DrawingConfig>,
        /// Pages whose content changed and whose visible tiles are therefore out of date. The
        /// next frame turns each into the set of tile keys below, and forgets the page here.
        pub stale_pages: RefCell<HashSet<u32>>,
        /// Tiles that are painted but out of date: asked for again every frame until the render
        /// that replaces them arrives, which is what makes an abandoned batch heal itself.
        pub stale_tiles: RefCell<HashSet<TileKey>>,
        /// The stroke being drawn, and any drawn before it whose render has not arrived yet.
        /// The live one, if there is one, is last.
        ///
        // ponytail: a stroke leaves this list when a tile of its page lands, so the list is as
        // long as the strokes drawn inside one render — one or two in practice. It cannot leak:
        // a stroke with no render to replace it is one that still has to be painted.
        pub strokes: RefCell<Vec<super::Stroke>>,
        /// What the Adjust tool can take hold of, per page it has been told about.
        pub inks: RefCell<HashMap<usize, Vec<accent_core::pdf::InkShape>>>,
        pub adjust: RefCell<Option<super::Selected>>,
        /// A move went out: the stroke comes back at the end of its page's `/Annots`, so the
        /// next list of that page selects its last entry.
        pub reselect: Cell<bool>,
        pub current_mark: Cell<Option<(usize, usize)>>,
        /// What was asked for last, so an unchanged viewport does not re-ask on every frame.
        pub asked: RefCell<Vec<Want>>,
        /// The scale and colour scheme [`Self::asked`] was for. Every event that makes the tiles
        /// on screen the wrong ones without changing *which* tiles are wanted goes through here
        /// — a zoom step, a fit mode, a resize, a theme change, entering presentation mode — and
        /// without it the page keeps painting its blurry stand-in and never asks again.
        pub asked_for: Cell<(u32, bool)>,
        pub page: Cell<usize>,
        pub pointer: Cell<(f64, f64)>,
        /// The fraction of a wheel notch a smooth-scroll device has sent so far.
        pub scroll_accum: Cell<f64>,
        pub on_wants: RefCell<Option<Wants>>,
        pub on_reply: RefCell<Option<OnReply>>,
        pub on_select: RefCell<Option<OnSelect>>,
        pub on_goto: RefCell<Option<Page>>,
        pub on_pressed: RefCell<Option<Coords>>,
        pub on_clicked: RefCell<Option<Coords>>,
        pub on_ink: RefCell<Option<Stroke>>,
        pub on_erase: RefCell<Option<At>>,
        pub on_transform: RefCell<Option<Transform>>,
        pub on_motion: RefCell<Option<Coords>>,
        pub on_page: RefCell<Option<Page>>,
        pub on_zoom: RefCell<Option<Zoomed>>,
        pub on_lowres: RefCell<Option<Lowres>>,
    }

    // `gtk::ScrollablePolicy` has no `Default`, so the struct spells its own out.
    impl Default for PdfView {
        fn default() -> Self {
            PdfView {
                hadjustment: RefCell::new(None),
                vadjustment: RefCell::new(None),
                hscroll_policy: Cell::new(gtk::ScrollablePolicy::Minimum),
                vscroll_policy: Cell::new(gtk::ScrollablePolicy::Minimum),
                adj_handlers: RefCell::new([None, None]),
                sizes: RefCell::new(Vec::new()),
                layout: RefCell::new(super::Layout::default()),
                cache: OnceCell::new(),
                zoom: Cell::new(PdfZoom::default()),
                dark: Cell::new(false),
                thumbnails: Cell::new(false),
                marks: RefCell::new(HashMap::new()),
                highlights: RefCell::new(HashMap::new()),
                selection: RefCell::new(Vec::new()),
                drag_from: Cell::new(None),
                mode: Cell::new(super::Mode::default()),
                drag_mode: Cell::new(super::Mode::default()),
                style: RefCell::new(accent_core::config::DrawingConfig::default()),
                stale_pages: RefCell::new(HashSet::new()),
                stale_tiles: RefCell::new(HashSet::new()),
                strokes: RefCell::new(Vec::new()),
                inks: RefCell::new(HashMap::new()),
                adjust: RefCell::new(None),
                reselect: Cell::new(false),
                current_mark: Cell::new(None),
                asked: RefCell::new(Vec::new()),
                asked_for: Cell::new((0, false)),
                page: Cell::new(0),
                pointer: Cell::new((0.0, 0.0)),
                scroll_accum: Cell::new(0.0),
                on_wants: RefCell::new(None),
                on_reply: RefCell::new(None),
                on_select: RefCell::new(None),
                on_goto: RefCell::new(None),
                on_pressed: RefCell::new(None),
                on_clicked: RefCell::new(None),
                on_ink: RefCell::new(None),
                on_erase: RefCell::new(None),
                on_transform: RefCell::new(None),
                on_motion: RefCell::new(None),
                on_page: RefCell::new(None),
                on_zoom: RefCell::new(None),
                on_lowres: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PdfView {
        const NAME: &'static str = "AccentPdfView";
        type Type = super::PdfView;
        type ParentType = gtk::Widget;
        type Interfaces = (gtk::Scrollable,);
    }

    impl PdfView {
        fn adopt_h(&self, adjustment: Option<gtk::Adjustment>) {
            self.adopt(0, adjustment);
        }

        fn adopt_v(&self, adjustment: Option<gtk::Adjustment>) {
            self.adopt(1, adjustment);
        }

        /// Follow an adjustment: redraw when it moves, and drop the handler on the old one.
        fn adopt(&self, slot: usize, adjustment: Option<gtk::Adjustment>) {
            let obj = self.obj().clone();
            let old = match slot {
                0 => self.hadjustment.replace(adjustment.clone()),
                _ => self.vadjustment.replace(adjustment.clone()),
            };
            if let (Some(old), Some(id)) = (old, self.adj_handlers.borrow_mut()[slot].take()) {
                old.disconnect(id);
            }
            if let Some(adjustment) = adjustment {
                let id = adjustment.connect_value_changed(move |_| {
                    obj.queue_draw();
                    obj.imp().notice_page();
                });
                self.adj_handlers.borrow_mut()[slot] = Some(id);
            }
            self.obj().queue_allocate();
        }

        /// Report the page being read when it changes, for the header and the thumbnail frame.
        fn notice_page(&self) {
            let page = self.obj().current_page();
            if self.page.replace(page) != page {
                self.obj().queue_draw();
                if let Some(f) = self.on_page.borrow().as_ref() {
                    f(page);
                }
            }
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for PdfView {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj().clone();
            obj.set_overflow(gtk::Overflow::Hidden);
            obj.set_focusable(true);
            obj.set_hexpand(true);
            obj.set_vexpand(true);

            // Ctrl+wheel zooms around the pointer. Bubble phase, ahead of the scrolled window's
            // own controller, which does not filter Ctrl and would scroll as well.
            let scroll =
                gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
            scroll.connect_scroll(glib::clone!(
                #[weak]
                obj,
                #[upgrade_or]
                glib::Propagation::Proceed,
                move |controller, _, dy| {
                    // The strip is a view of the same document, so its own controller would zoom
                    // the thumbnails instead of the page being read.
                    if obj.imp().thumbnails.get()
                        || !controller
                            .current_event_state()
                            .contains(gdk::ModifierType::CONTROL_MASK)
                    {
                        return glib::Propagation::Proceed;
                    }
                    let steps = crate::wheel_steps(&obj.imp().scroll_accum, dy);
                    for _ in 0..steps.abs() {
                        obj.zoom_step(steps > 0, Some(obj.imp().pointer.get()));
                    }
                    glib::Propagation::Stop
                }
            ));
            obj.add_controller(scroll);

            let zoom = gtk::GestureZoom::new();
            zoom.connect_scale_changed(glib::clone!(
                #[weak]
                obj,
                move |gesture, scale| {
                    // Only past a threshold, or the smallest tremor on a touchpad re-renders.
                    if !(0.9..1.1).contains(&scale) {
                        obj.zoom_step(scale < 1.0, gesture.bounding_box_center());
                    }
                }
            ));
            obj.add_controller(zoom);

            let motion = gtk::EventControllerMotion::new();
            motion.connect_motion(glib::clone!(
                #[weak]
                obj,
                move |_, x, y| {
                    obj.imp().pointer.set((x, y));
                    let handler = obj.imp().on_motion.borrow();
                    if let Some(f) = handler.as_ref() {
                        f(&obj, x, y);
                    }
                }
            ));
            obj.add_controller(motion);

            // Dragging over the page selects the text under it. Claimed on the first motion
            // rather than on the press, so a plain click still reaches the link handler below.
            let drag = gtk::GestureDrag::new();
            drag.connect_drag_begin(glib::clone!(
                #[weak]
                obj,
                move |gesture, x, y| {
                    obj.imp().drag_from.set(Some((x, y)));
                    // A pen or an eraser claims the sequence at once, unlike a selection, which
                    // waits to see whether the pointer moves: a stroke that let the scrolled
                    // window have the first few pixels would scroll the page under the hand.
                    let mode = obj.effective_mode(gesture);
                    obj.imp().drag_mode.set(mode);
                    match mode {
                        super::Mode::Select => {}
                        mode if mode.draws() => {
                            let Some((page, _, _)) = obj.page_point(x, y) else {
                                return;
                            };
                            gesture.set_state(gtk::EventSequenceState::Claimed);
                            let at = obj.point_on(page, x, y);
                            obj.imp().strokes.borrow_mut().push(super::Stroke {
                                page,
                                points: vec![at],
                                tool: mode,
                                done: false,
                            });
                        }
                        super::Mode::Adjust => {
                            if obj.adjust_press(x, y) {
                                gesture.set_state(gtk::EventSequenceState::Claimed);
                            }
                        }
                        _ => {
                            gesture.set_state(gtk::EventSequenceState::Claimed);
                            obj.erase_at(x, y);
                        }
                    }
                }
            ));
            drag.connect_drag_update(glib::clone!(
                #[weak]
                obj,
                move |gesture, dx, dy| {
                    let Some((x, y)) = obj.imp().drag_from.get() else {
                        return;
                    };
                    match obj.imp().drag_mode.get() {
                        super::Mode::Select => {
                            // A few pixels of travel is a click with a shaky hand, not a
                            // selection.
                            if dx.abs() < 3.0 && dy.abs() < 3.0 {
                                return;
                            }
                            gesture.set_state(gtk::EventSequenceState::Claimed);
                            obj.select_between(x, y, x + dx, y + dy);
                        }
                        mode if mode.draws() => {
                            // The page is whichever one the stroke began on: a hand that runs
                            // over the edge keeps drawing on the paper it started on.
                            let page = match obj.imp().strokes.borrow().last() {
                                Some(stroke) if !stroke.done => stroke.page,
                                _ => return,
                            };
                            let at = obj.point_on(page, x + dx, y + dy);
                            if let Some(stroke) = obj.imp().strokes.borrow_mut().last_mut()
                                && !stroke.done
                            {
                                // A shape is its two ends, the press and wherever the hand is.
                                if mode.shapes() {
                                    let anchor = stroke.points[0];
                                    stroke.points.truncate(1);
                                    stroke.points.push(match mode {
                                        super::Mode::Line => super::snap(anchor, at),
                                        _ => at,
                                    });
                                } else {
                                    stroke.points.push(at);
                                }
                            }
                            obj.queue_draw();
                        }
                        super::Mode::Adjust => obj.adjust_drag(dx, dy),
                        _ => obj.erase_at(x + dx, y + dy),
                    }
                }
            ));
            drag.connect_drag_end(glib::clone!(
                #[weak]
                obj,
                move |_, dx, dy| {
                    let from = obj.imp().drag_from.replace(None);
                    let mode = obj.imp().drag_mode.get();
                    if mode == super::Mode::Adjust {
                        return obj.adjust_release();
                    }
                    if mode.draws() {
                        let mut strokes = obj.imp().strokes.borrow_mut();
                        // A click in a shape mode is no shape, and leaves nothing behind waiting
                        // for a tile that will never carry it.
                        let click = matches!(strokes.last(), Some(s) if !s.done && mode.shapes()
                            && super::shape_of(mode, s.points[0], s.points[s.points.len() - 1]).is_none());
                        if click {
                            strokes.pop();
                        }
                        // The stroke stays painted until a tile carries it, so the page never
                        // blinks between the hand letting go and pdfium answering.
                        let finished = match strokes.last_mut() {
                            Some(stroke) if !stroke.done => {
                                stroke.done = true;
                                Some((stroke.page, stroke.points.clone()))
                            }
                            _ => None,
                        };
                        drop(strokes);
                        if let Some((page, points)) = finished
                            && let Some(f) = obj.imp().on_ink.borrow().as_ref()
                        {
                            f(page, points);
                        }
                        return;
                    }
                    // The same few pixels the update handler calls a shaky hand rather than a
                    // selection: what is left is a click, and a click can be on a highlight.
                    if let Some((x, y)) = from
                        && dx.abs() < 3.0
                        && dy.abs() < 3.0
                        && !obj.imp().thumbnails.get()
                        && let Some(f) = obj.imp().on_clicked.borrow().as_ref()
                    {
                        f(&obj, x, y);
                    }
                }
            ));
            obj.add_controller(drag);

            let click = gtk::GestureClick::builder().button(0).build();
            click.connect_pressed(glib::clone!(
                #[weak]
                obj,
                move |gesture, _, x, y| {
                    obj.grab_focus();
                    // While a pen is out, a press is the start of a mark, not a link to follow.
                    if obj.effective_mode(gesture) != super::Mode::Select {
                        return;
                    }
                    match gesture.current_button() {
                        // A thumbnail is a button: clicking one goes to its page.
                        1 if obj.imp().thumbnails.get() => {
                            let page = obj.page_point(x, y).map(|(page, _, _)| page);
                            let handler = obj.imp().on_goto.borrow();
                            if let (Some(page), Some(f)) = (page, handler.as_ref()) {
                                f(page);
                            }
                        }
                        1 => {
                            let handler = obj.imp().on_pressed.borrow();
                            if let Some(f) = handler.as_ref() {
                                f(&obj, x, y);
                            }
                        }
                        _ => {}
                    }
                }
            ));
            obj.add_controller(click);
        }
    }

    impl WidgetImpl for PdfView {
        /// The content decides its own size, and the scrolled window gives it what it has.
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (0, 0, -1, -1)
        }

        fn size_allocate(&self, _width: i32, _height: i32, _baseline: i32) {
            self.obj().relayout();
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let layout = self.layout.borrow().clone();
            if layout.pages.is_empty() {
                return;
            }
            let (ox, oy) = obj.scroll_offset();
            let (_, vh) = (f64::from(obj.width()), f64::from(obj.height()));
            let dark = self.dark.get();
            let sf = obj.scale_factor().max(1);
            let device_scale = layout.scale * sf as f32;
            let scale_milli = (device_scale * 1000.0).round() as u32;
            let thumbnails = self.thumbnails.get();

            snapshot.save();
            snapshot.translate(&graphene::Point::new(-ox as f32, -oy as f32));

            let frame = obj.color();
            let accent = adw::StyleManager::default().accent_color_rgba();
            let mut wanted: Vec<Want> = Vec::new();
            let cache = obj.cache();
            let marks = self.marks.borrow();
            let highlights = self.highlights.borrow();
            let strokes = self.strokes.borrow();
            let adjust = self.adjust.borrow();
            let selection = self.selection.borrow();
            for (index, rect) in layout.pages.iter().enumerate() {
                // One viewport of prefetch above and below, so scrolling meets ready tiles.
                let visible =
                    f64::from(rect.y + rect.h) >= oy - vh && f64::from(rect.y) <= oy + vh + vh;
                if !visible {
                    continue;
                }
                let bounds = graphene::Rect::new(rect.x, rect.y, rect.w, rect.h);
                // The page's own paper, so a tile that has not arrived is not a hole.
                snapshot.append_color(&paper(dark), &bounds);

                let page = index as u32;
                let low = cache.borrow_mut().lowres(page, dark);
                let mut missing = false;
                // A page whose content changed becomes the set of tiles that are out of date,
                // once, here — this is where what is actually on screen is known.
                let refreshing = self.stale_pages.borrow_mut().remove(&page);
                if !thumbnails {
                    let device_w = (rect.w * sf as f32).round() as i32;
                    let device_h = (rect.h * sf as f32).round() as i32;
                    for ty in 0..tiles_across(device_h) {
                        for tx in 0..tiles_across(device_w) {
                            let key = TileKey {
                                page,
                                scale_milli,
                                tx: tx as u16,
                                ty: ty as u16,
                                dark,
                            };
                            if refreshing {
                                self.stale_tiles.borrow_mut().insert(key);
                            }
                            let tile = cache.borrow_mut().get(&key);
                            let want = Want {
                                page,
                                tx: tx as u16,
                                ty: ty as u16,
                            };
                            match tile {
                                Some(texture) => {
                                    let x = rect.x + (tx * TILE) as f32 / sf as f32;
                                    let y = rect.y + (ty * TILE) as f32 / sf as f32;
                                    let w = texture.width() as f32 / sf as f32;
                                    let h = texture.height() as f32 / sf as f32;
                                    snapshot
                                        .append_texture(&texture, &graphene::Rect::new(x, y, w, h));
                                    // Painted, and still the old render: ask again, and keep
                                    // asking until the replacement lands, so a batch pushed
                                    // aside by a scroll is picked up by the next one.
                                    if self.stale_tiles.borrow().contains(&key) {
                                        wanted.push(want);
                                    }
                                }
                                None => {
                                    missing = true;
                                    wanted.push(want);
                                }
                            }
                        }
                    }
                }
                // Over the tiles rather than under them: painting it every frame would cost a
                // scaled draw per page, and a finished page has nothing missing to cover.
                match (thumbnails || missing, low) {
                    (true, Some(low)) => {
                        snapshot.append_scaled_texture(&low, gsk::ScalingFilter::Linear, &bounds);
                    }
                    // Not even a stand-in yet: ask for one. `u16::MAX` is the whole page.
                    (true, None) => wanted.push(Want {
                        page,
                        tx: u16::MAX,
                        ty: u16::MAX,
                    }),
                    (false, _) => {}
                }

                // A hairline, so a white page on a light background still reads as a page.
                snapshot.append_border(
                    &gsk::RoundedRect::from_rect(bounds, 0.0),
                    &[1.0; 4],
                    &[edge(frame); 4],
                );

                if thumbnails && self.page.get() == index {
                    snapshot.append_border(
                        &gsk::RoundedRect::from_rect(bounds, 0.0),
                        &[2.0; 4],
                        &[accent; 4],
                    );
                }

                // Under the selection and the search marks: a highlight is what the page says,
                // the other two are what the reader is doing to it right now.
                if let Some(page_highlights) = highlights.get(&index) {
                    let colour = gdk::RGBA::new(accent.red(), accent.green(), accent.blue(), 0.2);
                    for quad in page_highlights.iter().flat_map(|(quads, _)| quads) {
                        snapshot.append_color(
                            &colour,
                            &graphene::Rect::new(
                                rect.x + quad.left * layout.scale,
                                rect.y + quad.top * layout.scale,
                                quad.width() * layout.scale,
                                quad.height() * layout.scale,
                            ),
                        );
                    }
                }
                if let Some((_, boxes)) = selection.iter().find(|(at, _)| *at == index) {
                    let colour = gdk::RGBA::new(accent.red(), accent.green(), accent.blue(), 0.35);
                    for glyph in boxes {
                        snapshot.append_color(
                            &colour,
                            &graphene::Rect::new(
                                rect.x + glyph.left * layout.scale,
                                rect.y + glyph.top * layout.scale,
                                glyph.width() * layout.scale,
                                glyph.height() * layout.scale,
                            ),
                        );
                    }
                }
                for stroke in strokes.iter().filter(|s| s.page == index) {
                    let builder = gsk::PathBuilder::new();
                    let point = |&(x, y): &(f32, f32)| {
                        graphene::Point::new(rect.x + x * layout.scale, rect.y + y * layout.scale)
                    };
                    let points = &stroke.points;
                    match (stroke.tool, points.as_slice()) {
                        (super::Mode::Rect, &[a, b]) => {
                            let (p, q) = (point(&a), point(&b));
                            builder.add_rect(&graphene::Rect::new(
                                p.x().min(q.x()),
                                p.y().min(q.y()),
                                (p.x() - q.x()).abs(),
                                (p.y() - q.y()).abs(),
                            ));
                        }
                        (super::Mode::Circle, &[a, b]) => {
                            let radius = (b.0 - a.0).hypot(b.1 - a.1) * layout.scale;
                            builder.add_circle(&point(&a), radius);
                        }
                        _ => {
                            if let Some(first) = points.first() {
                                builder.move_to(point(first).x(), point(first).y());
                                for p in &points[1..] {
                                    builder.line_to(point(p).x(), point(p).y());
                                }
                                // A stroke of one point is a dot, which a round cap draws from
                                // a zero-length line.
                                if points.len() == 1 {
                                    builder.line_to(point(first).x(), point(first).y());
                                }
                            }
                        }
                    }
                    // The tool's own style, so what the hand sees is what the render puts on
                    // the page. The blend mode is not reproduced here: over paper at this alpha
                    // it reads the same, and only the render is kept.
                    let style = obj.ink_style(stroke.tool);
                    let [r, g, b, a] = style.rgba.map(|v| f32::from(v) / 255.0);
                    let colour = gdk::RGBA::new(r, g, b, a);
                    let stroke_style = gsk::Stroke::new(style.width * layout.scale);
                    stroke_style.set_line_cap(gsk::LineCap::Round);
                    stroke_style.set_line_join(gsk::LineJoin::Round);
                    snapshot.append_stroke(&builder.to_path(), &stroke_style, &colour);
                }
                // The stroke the Adjust tool holds: its box with eight handles, and while the
                // hand is on it, a ghost of the stroke where the drag has taken it.
                if let Some(a) = adjust.as_ref().filter(|a| a.page == index) {
                    let point = |p: (f32, f32)| {
                        let (x, y) = accent_core::pdf::apply(a.matrix, p);
                        graphene::Point::new(rect.x + x * layout.scale, rect.y + y * layout.scale)
                    };
                    if a.handle.is_some()
                        && let Some(first) = a.points.first()
                    {
                        let builder = gsk::PathBuilder::new();
                        builder.move_to(point(*first).x(), point(*first).y());
                        for &p in &a.points[1..] {
                            builder.line_to(point(p).x(), point(p).y());
                        }
                        let [r, g, b, _] = a.style.rgba;
                        let colour = gdk::RGBA::new(
                            f32::from(r) / 255.0,
                            f32::from(g) / 255.0,
                            f32::from(b) / 255.0,
                            0.6,
                        );
                        let ghost = gsk::Stroke::new(a.style.width * layout.scale);
                        ghost.set_line_cap(gsk::LineCap::Round);
                        ghost.set_line_join(gsk::LineJoin::Round);
                        snapshot.append_stroke(&builder.to_path(), &ghost, &colour);
                    }
                    let (tl, br) = (
                        point((a.bounds.left, a.bounds.top)),
                        point((a.bounds.right, a.bounds.bottom)),
                    );
                    let (l, t) = (tl.x().min(br.x()), tl.y().min(br.y()));
                    let (r, b) = (tl.x().max(br.x()), tl.y().max(br.y()));
                    let frame = graphene::Rect::new(l, t, r - l, b - t);
                    snapshot.append_border(
                        &gsk::RoundedRect::from_rect(frame, 0.0),
                        &[1.0; 4],
                        &[accent; 4],
                    );
                    let (cx, cy) = ((l + r) / 2.0, (t + b) / 2.0);
                    for (hx, hy) in [
                        (l, t),
                        (cx, t),
                        (r, t),
                        (l, cy),
                        (r, cy),
                        (l, b),
                        (cx, b),
                        (r, b),
                    ] {
                        let square = graphene::Rect::new(
                            hx - super::HANDLE / 2.0,
                            hy - super::HANDLE / 2.0,
                            super::HANDLE,
                            super::HANDLE,
                        );
                        snapshot.append_color(&accent, &square);
                    }
                }
                if let Some(page_marks) = marks.get(&index) {
                    for (n, mark) in page_marks.iter().enumerate() {
                        let alpha = match self.current_mark.get() == Some((index, n)) {
                            true => 0.6,
                            false => 0.3,
                        };
                        let colour =
                            gdk::RGBA::new(accent.red(), accent.green(), accent.blue(), alpha);
                        snapshot.append_color(
                            &colour,
                            &graphene::Rect::new(
                                rect.x + mark.left * layout.scale,
                                rect.y + mark.top * layout.scale,
                                mark.width() * layout.scale,
                                mark.height() * layout.scale,
                            ),
                        );
                    }
                }
            }
            drop(marks);
            drop(selection);
            snapshot.restore();

            // Asked for once per change, not once per frame: a scroll that reveals nothing new
            // must not re-send the same list. The scale and the scheme are part of "the same",
            // because the same tiles at another one are a different render.
            let stamp = (scale_milli, dark);
            if !wanted.is_empty()
                && (self.asked_for.get() != stamp || *self.asked.borrow() != wanted)
            {
                self.asked_for.set(stamp);
                *self.asked.borrow_mut() = wanted.clone();
                let handler = self.on_wants.borrow();
                if let Some(f) = handler.as_ref() {
                    f(&obj, device_scale, dark, wanted);
                }
            }
        }
    }

    impl ScrollableImpl for PdfView {}
}

/// A rendered image as something GTK can paint.
pub fn texture(image: accent_core::pdf::RgbaImage) -> gdk::MemoryTexture {
    let stride = image.width as usize * 4;
    gdk::MemoryTexture::new(
        image.width as i32,
        image.height as i32,
        gdk::MemoryFormat::R8g8b8a8,
        &glib::Bytes::from_owned(image.data),
        stride,
    )
}

/// How many tiles cover `pixels`.
fn tiles_across(pixels: i32) -> i32 {
    (pixels + TILE - 1) / TILE
}

/// The colour a page's paper is drawn in before its tiles arrive, matching what the renderer will
/// produce so nothing flashes when they do.
fn paper(dark: bool) -> gdk::RGBA {
    match crate::theme::view_bg(dark).parse::<gdk::RGBA>() {
        Ok(colour) => colour,
        Err(_) => gdk::RGBA::WHITE,
    }
}

/// The hairline around a page: the foreground at a low alpha, like every other derived colour.
fn edge(fg: gdk::RGBA) -> gdk::RGBA {
    gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.15)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn letter(n: usize) -> Vec<(f32, f32)> {
        vec![(612.0, 792.0); n]
    }

    #[test]
    fn pages_stack_with_a_gap_and_centre_in_the_viewport() {
        let out = layout(&letter(3), 1.0, 1000.0);
        assert_eq!(out.pages.len(), 3);
        assert_eq!(out.pages[0].y, GAP);
        assert_eq!(out.pages[1].y, GAP + 792.0 + GAP);
        // Centred: the same margin either side.
        assert_eq!(out.pages[0].x, (1000.0 - 612.0) / 2.0);
        assert_eq!(out.width, 1000.0);
        assert_eq!(out.height, GAP + (792.0 + GAP) * 3.0);
    }

    #[test]
    fn a_page_wider_than_the_viewport_sets_the_content_width() {
        let out = layout(&letter(1), 2.0, 500.0);
        assert_eq!(out.width, 612.0 * 2.0 + GAP * 2.0);
        assert_eq!(out.pages[0].x, GAP);
    }

    #[test]
    fn fit_page_takes_the_smaller_axis() {
        let sizes = letter(2);
        // Wide and short: height is what binds.
        let scale = fit_scale(&sizes, PdfZoom::FitPage, 2000.0, 400.0);
        assert!((scale - (400.0 - GAP * 2.0) / 792.0).abs() < 1e-6);
        let width = fit_scale(&sizes, PdfZoom::FitWidth, 2000.0, 400.0);
        assert!(width > scale);
    }

    #[test]
    fn fit_page_puts_a_whole_page_on_screen() {
        let sizes = letter(3);
        let (vw, vh) = (900.0, 700.0);
        let fitted = layout(&sizes, fit_scale(&sizes, PdfZoom::FitPage, vw, vh), vw);
        // The reader was halfway down page two when Fit Page was asked for.
        let was = Anchor {
            page: 1,
            u: 0.0,
            v: 0.5,
        };
        let page = fitted.pages[1];
        let whole = |top: f64| {
            top <= f64::from(page.y) && f64::from(page.y + page.h) <= top + f64::from(vh)
        };
        let (_, top) = offset_of(&fitted, resume_at(PdfZoom::FitPage, was)).expect("page two");
        assert!(whole(top), "page two is not wholly on screen from {top}");
        // Resuming where the reader was, which is what every other zoom does, leaves half of it
        // above the viewport and half of page three below: that is what Fit Page looked like.
        let (_, kept) = offset_of(&fitted, was).expect("page two");
        assert!(
            !whole(kept),
            "nothing to fix: page two already fits from {kept}"
        );
        assert_eq!(resume_at(PdfZoom::FitWidth, was), was);
    }

    fn scale(zoom: PdfZoom) -> f64 {
        let PdfZoom::Scale(zoom) = zoom else {
            panic!("a step is always a fixed scale")
        };
        zoom
    }

    #[test]
    fn zoom_steps_in_tenths_and_clamps() {
        assert_eq!(scale(stepped(1.0, false)), 1.1);
        assert_eq!(scale(stepped(1.0, true)), 0.9);
        // A page fitted to the window sits off a tenth: the next one, not a tenth further.
        assert_eq!(scale(stepped(1.37, false)), 1.4);
        assert_eq!(scale(stepped(1.37, true)), 1.3);
        assert_eq!(clamp_scale(1000.0), 8.0 * PT_TO_PX);
        assert_eq!(clamp_scale(0.0), 0.1 * PT_TO_PX);
    }

    #[test]
    fn a_step_reaches_both_ends_of_the_range_without_sticking() {
        // Stepping used to be read back out of the laid-out `f32` scale, where past 230 % the
        // rounding made every step land on the zoom the page was already at.
        assert_eq!(scale(stepped(2.3, false)), 2.4);
        let mut zoom = MIN_SCALE;
        for _ in 0..200 {
            let next = scale(stepped(zoom, false));
            assert!(next > zoom || next == MAX_SCALE, "stuck at {zoom}");
            zoom = next;
        }
        assert_eq!(zoom, MAX_SCALE);
        for _ in 0..200 {
            let next = scale(stepped(zoom, true));
            assert!(next < zoom || next == MIN_SCALE, "stuck at {zoom}");
            zoom = next;
        }
        assert_eq!(zoom, MIN_SCALE);
    }

    #[test]
    fn an_anchor_is_the_same_place_after_a_resize() {
        let sizes = letter(5);
        let fit = |width: f32| {
            layout(
                &sizes,
                fit_scale(&sizes, PdfZoom::FitWidth, width, 700.0),
                width,
            )
        };
        let (wide, narrow) = (fit(1000.0), fit(500.0));
        let anchor = Anchor {
            page: 2,
            u: 0.0,
            v: 1.0 / 3.0,
        };
        let (_, was) = offset_of(&wide, anchor).unwrap();
        let (_, now) = offset_of(&narrow, anchor).unwrap();
        // Half the column is half the height, so the raw offset means something else entirely:
        // it lands two pages further down. The anchor is what survives the resize.
        assert!(now < was);
        assert_eq!(anchor_at(&narrow, 0.0, now).page, anchor.page);
        assert!((anchor_at(&narrow, 0.0, now).v - anchor.v).abs() < 1e-4);
        assert_ne!(anchor_at(&narrow, 0.0, was).page, anchor.page);
    }

    #[test]
    fn tiles_cover_the_page_including_a_partial_last_one() {
        assert_eq!(tiles_across(TILE), 1);
        assert_eq!(tiles_across(TILE + 1), 2);
        assert_eq!(tiles_across(0), 0);
    }

    #[test]
    fn an_anchor_survives_a_document_that_lost_pages() {
        let anchor = Anchor {
            page: 9,
            u: 0.5,
            v: 0.25,
        };
        assert_eq!(anchor.clamped(4).page, 3);
        assert_eq!(anchor.clamped(4).v, 0.25);
    }

    #[test]
    fn a_line_snaps_to_the_axis_within_seven_degrees() {
        assert_eq!(snap((0.0, 0.0), (100.0, 10.0)), (100.0, 0.0));
        assert_eq!(snap((0.0, 0.0), (100.0, 20.0)), (100.0, 20.0));
        assert_eq!(snap((0.0, 0.0), (5.0, 100.0)), (0.0, 100.0));
    }

    #[test]
    fn a_shape_is_normalised_and_a_click_is_not_one() {
        use accent_core::pdf::{Rect, Shape};
        let rect = shape_of(Mode::Rect, (50.0, 50.0), (10.0, 20.0));
        assert_eq!(
            rect,
            Some(Shape::Rect(Rect {
                left: 10.0,
                top: 20.0,
                right: 50.0,
                bottom: 50.0
            }))
        );
        let circle = shape_of(Mode::Circle, (0.0, 0.0), (3.0, 4.0));
        assert_eq!(
            circle,
            Some(Shape::Circle {
                centre: (0.0, 0.0),
                radius: 5.0
            })
        );
        assert_eq!(shape_of(Mode::Rect, (7.0, 7.0), (7.0, 7.5)), None);
        assert_eq!(shape_of(Mode::Pen, (0.0, 0.0), (9.0, 9.0)), None);
    }

    #[test]
    fn handles_map_to_matrices() {
        use accent_core::pdf::{Rect, apply};
        let b = Rect {
            left: 10.0,
            top: 20.0,
            right: 50.0,
            bottom: 60.0,
        };
        assert_eq!(handle_at(b, (50.0, 60.0), 3.0), Some(Handle::BottomRight));
        assert_eq!(handle_at(b, (30.0, 20.0), 3.0), Some(Handle::Top));
        assert_eq!(handle_at(b, (30.0, 40.0), 3.0), Some(Handle::Move));
        assert_eq!(handle_at(b, (100.0, 100.0), 3.0), None);

        let m = drag_matrix(Handle::Right, b, 40.0, 0.0);
        assert_eq!(apply(m, (50.0, 33.0)), (90.0, 33.0));
        assert_eq!(apply(m, (10.0, 33.0)), (10.0, 33.0));
        let m = drag_matrix(Handle::BottomRight, b, 40.0, 40.0);
        assert_eq!(apply(m, (50.0, 60.0)), (90.0, 100.0));
        assert_eq!(apply(m, (10.0, 20.0)), (10.0, 20.0));
        let m = drag_matrix(Handle::Move, b, 3.0, 4.0);
        assert_eq!(apply(m, (10.0, 20.0)), (13.0, 24.0));
        // A horizontal line has no height to stretch, and its edge drag leaves it a line.
        let flat = Rect {
            left: 0.0,
            top: 5.0,
            right: 10.0,
            bottom: 5.0,
        };
        assert_eq!(
            drag_matrix(Handle::Bottom, flat, 0.0, 9.0),
            accent_core::pdf::IDENTITY
        );
    }

    /// Ten entries of 10 bytes against a budget of 100: nothing goes until the eleventh, and then
    /// enough of the oldest go at once to leave room for three more.
    #[test]
    fn eviction_drops_the_least_recently_used_in_batches() {
        let entries =
            |n: u64| -> Vec<(u64, usize, u64)> { (0..n).map(|tick| (tick, 10, tick)).collect() };
        assert!(overflowing(entries(10), 100, 100).is_empty());
        // 110 down to 75 or less: four of the ten, oldest first.
        assert_eq!(overflowing(entries(11), 110, 100), vec![0, 1, 2, 3]);
        // Freshly painted pages sort last, so they are the ones eviction never reaches.
        let mut used = entries(11);
        used[0].0 = 99;
        assert!(!overflowing(used, 110, 100).contains(&0));
    }
}
