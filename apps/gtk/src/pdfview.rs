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
use std::collections::HashMap;

/// Tile edge in device pixels. 512 is 1 MiB of RGBA, small enough that a scroll never waits on
/// one page-sized render and large enough that a screen is a handful of them.
pub const TILE: i32 = 512;

/// Width of the low-resolution stand-in, in device pixels. Also what the thumbnail strip paints.
pub const LOWRES_W: i32 = 256;

/// Between pages, and around the column. The 12 of DESIGN.md's spacing scale.
const GAP: f32 = 12.0;

/// Texture bytes held before the least recently used are dropped.
const BUDGET: usize = 256 << 20;

/// Points to CSS pixels at zoom 1.0. A PDF point is 1/72 inch and a CSS pixel 1/96.
const PT_TO_PX: f32 = 96.0 / 72.0;

const MIN_SCALE: f64 = 0.1;
const MAX_SCALE: f64 = 8.0;
/// One zoom step. Multiplicative, so stepping in and out returns to where it started.
const STEP: f64 = 1.2;

/// How the page is sized to the window. Defined in core, because the session remembers it.
pub use accent_core::config::PdfZoom;

/// The header readout for a zoom, or `None` while the mode speaks for itself.
pub fn zoom_label(zoom: PdfZoom) -> Option<String> {
    match zoom {
        PdfZoom::FitWidth => None,
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

/// Textures already rendered, dropped least-recently-used first once they outgrow [`BUDGET`].
#[derive(Default)]
pub struct Cache {
    tiles: HashMap<TileKey, (gdk::MemoryTexture, u64)>,
    /// One whole-page thumbnail per page and theme, which is what a page shows before its tiles
    /// arrive and what the thumbnail strip paints. Never evicted: they are small and always
    /// wanted.
    lowres: HashMap<(u32, bool), gdk::MemoryTexture>,
    bytes: usize,
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
        self.evict();
    }

    pub fn lowres(&self, page: u32, dark: bool) -> Option<gdk::MemoryTexture> {
        self.lowres.get(&(page, dark)).cloned()
    }

    pub fn insert_lowres(&mut self, page: u32, dark: bool, texture: gdk::MemoryTexture) {
        self.lowres.insert((page, dark), texture);
    }

    pub fn clear(&mut self) {
        self.tiles.clear();
        self.lowres.clear();
        self.bytes = 0;
    }

    /// Drop the oldest tiles until the cache is comfortably under budget, so eviction happens in
    /// batches rather than on every single insert once it is full.
    fn evict(&mut self) {
        if self.bytes <= BUDGET {
            return;
        }
        let mut ages: Vec<(u64, TileKey)> = self.tiles.iter().map(|(k, (_, t))| (*t, *k)).collect();
        ages.sort_unstable_by_key(|(tick, _)| *tick);
        for (_, key) in ages {
            if self.bytes * 4 <= BUDGET * 3 {
                break;
            }
            if let Some((texture, _)) = self.tiles.remove(&key) {
                self.bytes -= (texture.width() * texture.height() * 4) as usize;
            }
        }
    }
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
    Outline(Vec<accent_core::pdf::Outline>),
    /// One page's matches for the query identified by `query`; a later one abandons it.
    Found {
        query: u64,
        page: usize,
        hits: Vec<Vec<accent_core::pdf::Rect>>,
    },
    /// The file was re-read: these are its page sizes now.
    Reloaded(Vec<(f32, f32)>),
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

/// One zoom step in or out from `from`, as a fixed scale.
pub fn stepped(from: f32, out: bool) -> PdfZoom {
    let current = f64::from(from) / f64::from(PT_TO_PX);
    let next = match out {
        true => current / STEP,
        false => current * STEP,
    };
    // Rounded as well as clamped, so stepping never drifts into 1.4400000000000002.
    PdfZoom::Scale((next * 100.0).round() / 100.0)
}

/// A scale clamped to what is worth rendering: below the floor nothing is legible, above the
/// ceiling one page is hundreds of megabytes of tiles.
pub fn clamp_scale(scale: f32) -> f32 {
    scale.clamp((MIN_SCALE as f32) * PT_TO_PX, (MAX_SCALE as f32) * PT_TO_PX)
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
        let anchor = self.anchor();
        self.imp().zoom.set(zoom);
        self.relayout();
        self.scroll_to(anchor);
    }

    /// Zoom one step, keeping whatever is under `at` (a widget coordinate) where it is.
    pub fn zoom_step(&self, out: bool, at: Option<(f64, f64)>) {
        let scale = self.imp().layout.borrow().scale;
        self.zoom_around(stepped(scale, out), at);
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
        self.cache().borrow_mut().insert(key, texture, bytes);
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
            Reply::Lowres { page, dark, image } => self.insert_lowres(page, dark, texture(image)),
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

    /// Called on pointer motion, so the tab can show a hand over a link.
    pub fn connect_motion(&self, f: impl Fn(&PdfView, f64, f64) + 'static) {
        *self.imp().on_motion.borrow_mut() = Some(Box::new(f));
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

    /// The one match to draw more strongly than the rest.
    pub fn set_current_mark(&self, at: Option<(usize, usize)>) {
        self.imp().current_mark.set(at);
        self.queue_draw();
    }

    /// Where the reader is now.
    pub fn anchor(&self) -> Anchor {
        let layout = self.imp().layout.borrow();
        let (x, y) = self.scroll_offset();
        let page = self.page_at(y + 1.0);
        match layout.pages.get(page) {
            Some(rect) => Anchor {
                page,
                u: ((x - f64::from(rect.x)) / f64::from(rect.w.max(1.0))) as f32,
                v: ((y - f64::from(rect.y)) / f64::from(rect.h.max(1.0))) as f32,
            },
            None => Anchor::default(),
        }
    }

    /// Put the reader back where `anchor` says.
    pub fn scroll_to(&self, anchor: Anchor) {
        let layout = self.imp().layout.borrow().clone();
        let Some(rect) = layout.pages.get(anchor.page) else {
            return;
        };
        if let Some(hadj) = self.hadjustment() {
            hadj.set_value(f64::from(rect.x + anchor.u * rect.w));
        }
        if let Some(vadj) = self.vadjustment() {
            vadj.set_value(f64::from(rect.y + anchor.v * rect.h));
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
        self.page_at(middle)
    }

    /// Which page a content coordinate falls in, or the nearest one above it.
    fn page_at(&self, y: f64) -> usize {
        let layout = self.imp().layout.borrow();
        layout
            .pages
            .iter()
            .rposition(|rect| f64::from(rect.y) <= y)
            .unwrap_or(0)
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
    fn relayout(&self) {
        let (w, h) = (self.width(), self.height());
        if w <= 1 || h <= 1 {
            return;
        }
        let sizes = self.imp().sizes.borrow().clone();
        if sizes.is_empty() {
            return;
        }
        let scale = clamp_scale(fit_scale(&sizes, self.imp().zoom.get(), w as f32, h as f32));
        let layout = layout(&sizes, scale, w as f32);
        let (width, height) = (f64::from(layout.width), f64::from(layout.height));
        *self.imp().layout.borrow_mut() = layout;
        configure(self.hadjustment(), width, f64::from(w));
        configure(self.vadjustment(), height, f64::from(h));
        self.queue_draw();
    }
}

/// Point an adjustment at a content size without disturbing where it is scrolled to.
fn configure(adjustment: Option<gtk::Adjustment>, upper: f64, page: f64) {
    let Some(adjustment) = adjustment else {
        return;
    };
    let value = adjustment.value().min((upper - page).max(0.0));
    adjustment.configure(value, 0.0, upper, page * 0.1, page * 0.9, page);
}

mod imp {
    use super::*;
    use std::cell::OnceCell;

    type Wants = Box<dyn Fn(&super::PdfView, f32, bool, Vec<Want>)>;
    type Coords = Box<dyn Fn(&super::PdfView, f64, f64)>;
    type Page = Box<dyn Fn(usize)>;
    type OnReply = Box<dyn Fn(&super::PdfView, Reply)>;

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
        pub current_mark: Cell<Option<(usize, usize)>>,
        /// What was asked for last, so an unchanged viewport does not re-ask on every frame.
        pub asked: RefCell<Vec<Want>>,
        pub page: Cell<usize>,
        pub pointer: Cell<(f64, f64)>,
        pub on_wants: RefCell<Option<Wants>>,
        pub on_reply: RefCell<Option<OnReply>>,
        pub on_goto: RefCell<Option<Page>>,
        pub on_pressed: RefCell<Option<Coords>>,
        pub on_motion: RefCell<Option<Coords>>,
        pub on_page: RefCell<Option<Page>>,
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
                current_mark: Cell::new(None),
                asked: RefCell::new(Vec::new()),
                page: Cell::new(0),
                pointer: Cell::new((0.0, 0.0)),
                on_wants: RefCell::new(None),
                on_reply: RefCell::new(None),
                on_goto: RefCell::new(None),
                on_pressed: RefCell::new(None),
                on_motion: RefCell::new(None),
                on_page: RefCell::new(None),
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
                    if !controller
                        .current_event_state()
                        .contains(gdk::ModifierType::CONTROL_MASK)
                    {
                        return glib::Propagation::Proceed;
                    }
                    obj.zoom_step(dy > 0.0, Some(obj.imp().pointer.get()));
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

            let click = gtk::GestureClick::builder().button(0).build();
            click.connect_pressed(glib::clone!(
                #[weak]
                obj,
                move |gesture, _, x, y| {
                    obj.grab_focus();
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
                let low = cache.borrow().lowres(page, dark);
                let mut missing = false;
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
                            let tile = cache.borrow_mut().get(&key);
                            match tile {
                                Some(texture) => {
                                    let x = rect.x + (tx * TILE) as f32 / sf as f32;
                                    let y = rect.y + (ty * TILE) as f32 / sf as f32;
                                    let w = texture.width() as f32 / sf as f32;
                                    let h = texture.height() as f32 / sf as f32;
                                    snapshot
                                        .append_texture(&texture, &graphene::Rect::new(x, y, w, h));
                                }
                                None => {
                                    missing = true;
                                    wanted.push(Want {
                                        page,
                                        tx: tx as u16,
                                        ty: ty as u16,
                                    });
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
            snapshot.restore();

            // Asked for once per change, not once per frame: a scroll that reveals nothing new
            // must not re-send the same list.
            if !wanted.is_empty() && *self.asked.borrow() != wanted {
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
    fn zoom_steps_are_reversible_and_clamped() {
        let at = fit_scale(&letter(1), PdfZoom::Scale(1.0), 100.0, 100.0);
        let PdfZoom::Scale(inned) = stepped(at, false) else {
            panic!("a step is always a fixed scale")
        };
        assert_eq!(inned, 1.2);
        let PdfZoom::Scale(back) = stepped(at * 1.2, true) else {
            panic!("a step is always a fixed scale")
        };
        assert_eq!(back, 1.0);
        assert_eq!(clamp_scale(1000.0), 8.0 * PT_TO_PX);
        assert_eq!(clamp_scale(0.0), 0.1 * PT_TO_PX);
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
}
