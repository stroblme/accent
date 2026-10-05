//! The widget a PDF is read in: a scrollable column of pages painted from cached tiles.
//!
//! Nothing here opens a file or calls pdfium. The widget knows the page sizes, the zoom and the
//! textures it has been given; when it paints a page it has no tiles for, it asks for them and
//! draws a blurred low-resolution stand-in until they arrive. `tab.rs` owns the document and the
//! thread that answers.
//!
//! Two of these share one document: the reading view, and a narrow one beside it showing every
//! page as a thumbnail.
//!
//! `paint` draws the pages, `gesture` is what the pointer does on them, and `scroll` is where the
//! view is in the document.

mod gesture;
mod paint;
mod scroll;

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use super::cache::{Cache, TileKey, Want, texture};
use super::geometry::{Layout, PT_TO_PX, PdfZoom, Span, stepped};
use super::protocol::{Highlights, NamedInk, Pass, Reply};
use super::tools::{Mode, Selected, Stroke};
use crate::theme;

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
        let view: PdfView = glib::Object::new();
        // Ctrl+wheel zooms around the pointer, in the same step a chord takes. Bubble phase,
        // ahead of the scrolled window's own controller, which does not filter Ctrl and would
        // scroll as well. Neither it nor the pinch is on the thumbnail strip below, which fits
        // itself to its width: a wheel there is the scroll it has always been.
        crate::zoom::zoom_on_wheel(
            &view,
            gtk::PropagationPhase::Bubble,
            glib::clone!(
                #[weak]
                view,
                move |out| view.zoom_step(out, view.imp().over.get())
            ),
        );
        crate::zoom::zoom_on_pinch(
            &view,
            glib::clone!(
                #[weak]
                view,
                #[upgrade_or]
                1.0,
                move || view.zoom_factor()
            ),
            glib::clone!(
                #[weak]
                view,
                move |zoom, at| {
                    let zoom = PdfZoom::Scale(crate::zoom::clamp_scale(zoom));
                    if view.zoom() != zoom {
                        view.zoom_around(zoom, Some(at));
                    }
                }
            ),
        );
        view
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
        self.zoom_around(stepped(self.zoom_factor(), out), at);
    }

    /// The zoom as a multiple of the page's natural size, whatever the mode: what a step or a
    /// pinch starts from.
    fn zoom_factor(&self) -> f64 {
        match self.imp().zoom.get() {
            // Exact, so a step never has to be read back out of the laid-out `f32` scale.
            PdfZoom::Scale(zoom) => zoom,
            // A fit mode has no percentage of its own: step from wherever it left the page.
            _ => f64::from(self.imp().layout.borrow().scale) / f64::from(PT_TO_PX),
        }
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
            self.imp().paper.set(None);
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
        // A palette this dark that is not the palette it was: the paper moved with it.
        self.imp().paper.set(None);
        self.queue_draw();
    }

    /// The document's pages were put in, taken out or reordered, and `map` says where each went:
    /// what this view waits on for a page — a stroke to settle, a tile to replace — follows it.
    /// The cache is shared with the other view, so it is the tab's to move, once.
    pub fn repage(&self, map: impl Fn(usize) -> Option<usize>) {
        let page = |p: u32| map(p as usize).map(|p| p as u32);
        let imp = self.imp();
        let stale = std::mem::take(&mut *imp.stale_pages.borrow_mut());
        *imp.stale_pages.borrow_mut() = stale
            .into_iter()
            .filter_map(|(at, area)| Some((page(at)?, area)))
            .collect();
        let stale = std::mem::take(&mut *imp.stale_tiles.borrow_mut());
        *imp.stale_tiles.borrow_mut() = stale
            .into_iter()
            .filter_map(|key| {
                Some(TileKey {
                    page: page(key.page)?,
                    ..key
                })
            })
            .collect();
        imp.strokes
            .borrow_mut()
            .retain_mut(|stroke| match map(stroke.page) {
                Some(at) => {
                    stroke.page = at;
                    true
                }
                None => false,
            });
        self.queue_draw();
    }

    /// Where the pages are laid out, and how far down the view is scrolled: what the thumbnail
    /// strip places its buttons and its drop bar by.
    pub(super) fn placement(&self) -> (Layout, f64) {
        (self.imp().layout.borrow().clone(), self.scroll_offset().1)
    }

    /// Called with a page number when a click or a key asks to go somewhere.
    pub fn connect_goto(&self, f: impl Fn(usize) + 'static) {
        *self.imp().on_goto.borrow_mut() = Some(Box::new(f));
    }

    /// Called with a widget coordinate on every primary click, for link hit-testing.
    pub fn connect_pressed(&self, f: impl Fn(&PdfView, f64, f64) + 'static) {
        *self.imp().on_pressed.borrow_mut() = Some(Box::new(f));
    }

    /// Called when a press turns out to have been a click rather than the start of a drag, with
    /// the modifiers held.
    ///
    /// A highlight opens the note that holds it, and that must not fire on every drag that
    /// happens to begin inside one — so it waits for the release, unlike the link handler above,
    /// which answers on the press because following a link is what a press on one means.
    pub fn connect_clicked(&self, f: impl Fn(&PdfView, f64, f64, gdk::ModifierType) + 'static) {
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

    /// Called whenever [`PdfView::visible_pages`] changes.
    pub fn connect_shown(&self, f: impl Fn() + 'static) {
        *self.imp().on_shown.borrow_mut() = Some(Box::new(f));
    }

    /// Frame `page` in a thumbnail strip: the page the reading view is on, which the strip's own
    /// scroll has no say in.
    pub fn set_framed(&self, page: usize) {
        if self.imp().page.replace(page) != page {
            self.queue_draw();
        }
    }

    /// The page a thumbnail strip frames. Only `ACCENT_BENCH_PDF` reads it.
    #[cfg(feature = "bench")]
    pub fn framed(&self) -> usize {
        self.imp().page.get()
    }

    /// Rectangles to paint over the page, in page points, per page: the search matches.
    pub fn set_marks(&self, marks: HashMap<usize, Vec<accent_core::pdf::Rect>>) {
        *self.imp().marks.borrow_mut() = marks;
        self.queue_draw();
    }

    /// One page's matches, as a query finds them. The map is added to rather than rebuilt: a
    /// search reports page by page, and rebuilding it for each was the whole map per reply.
    pub fn add_marks(&self, page: usize, rects: Vec<accent_core::pdf::Rect>) {
        self.imp()
            .marks
            .borrow_mut()
            .entry(page)
            .or_default()
            .extend(rects);
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
        highlights
            .get(&page)?
            .iter()
            .find_map(|(quads, link)| quads.iter().any(|q| q.contains((px, py))).then_some(*link))
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
        // exactly the thing that will not happen. A stylus's next move puts its dot back.
        self.imp().dot.set(false);
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

    /// How a tool's stroke is drawn, from the preferences: the highlighter in its own width and
    /// colour, wide and translucent; everything else — the pen and the shapes — in the pen's.
    /// One width per stroke: a stylus reports pressure and this ignores it.
    ///
    // ponytail: uniform width because varying it means storing a width per point and drawing
    // the stroke as a filled outline rather than a stroked path. A `GestureStylus` reading
    // pressure is the upgrade.
    pub fn ink_style(&self, mode: Mode) -> accent_core::pdf::InkStyle {
        let c = self.imp().style.borrow();
        let (width, colour, alpha, multiply) = match mode.style_owner() {
            Mode::Highlighter => (
                c.highlighter_width,
                c.highlighter_color,
                theme::HIGHLIGHTER_ALPHA,
                true,
            ),
            _ => (c.pen_width, c.pen_color, 1.0, false),
        };
        let [r, g, b] = colour.unwrap_or_else(theme::accent_rgb);
        accent_core::pdf::InkStyle {
            width,
            rgba: [r, g, b, (alpha * 255.0) as u8],
            multiply,
        }
    }

    /// Called with a finished stroke: the page and its points, in that page's own points.
    pub fn connect_ink(&self, f: impl Fn(usize, Vec<(f32, f32)>) + 'static) {
        *self.imp().on_ink.borrow_mut() = Some(Box::new(f));
    }

    /// Called with a page, the id of a stroke on it the eraser passed over, what a partial eraser
    /// left of it, and whether the same drag took one before it.
    pub fn connect_erase(&self, f: impl Fn(usize, u32, Option<Pass>, bool) + 'static) {
        *self.imp().on_erase.borrow_mut() = Some(Box::new(f));
    }

    /// Called with a page, the id of a stroke on it and the map to move it by.
    pub fn connect_transform(&self, f: impl Fn(usize, u32, accent_core::pdf::Matrix) + 'static) {
        *self.imp().on_transform.borrow_mut() = Some(Box::new(f));
    }

    /// What one page holds for the eraser and the Adjust tool, read once the render thread had
    /// answered `erases` of this view's erases. A list read before the last of them landed is of
    /// the page before it — the view's own, taken out of and cut as it went, is truer — so it is
    /// let go; the erase itself sends one back. A selection on that page follows its stroke by id
    /// to wherever a move put it, or goes if the stroke did.
    pub fn set_inks(&self, page: usize, inks: Vec<NamedInk>, erases: u64) {
        if erases < self.imp().erases.get() {
            return;
        }
        {
            let mut adjust = self.imp().adjust.borrow_mut();
            if let Some(a) = adjust.as_ref()
                && a.page == page
            {
                let fresh = inks.iter().find(|(id, _)| *id == a.id);
                *adjust = fresh.map(|(id, i)| Selected {
                    page,
                    id: *id,
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

    /// Draw one page again, because what it holds changed — a stroke, an erase, an export.
    ///
    /// Deliberately not an eviction. What is on screen keeps being painted until its replacement
    /// arrives, so a stroke costs one re-render and no blank page in between.
    pub fn refresh_page(&self, page: usize, area: accent_core::pdf::Rect) {
        let (scale_milli, dark) = self.stamp();
        self.cache()
            .borrow_mut()
            .forget_page_except(page as u32, scale_milli, dark);
        // Widened where a stroke is already on the page: a box that only just reaches a tile
        // edge should still take that tile, and a point-sized change none at all is no change.
        let area = area.grow(1.0);
        let mut stale = self.imp().stale_pages.borrow_mut();
        let all = stale.entry(page as u32).or_insert(area);
        *all = all.union(area);
        drop(stale);
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

    /// The boxes the selection paints, as [`PdfView::set_selection`] was handed them. Only drills
    /// ask.
    #[cfg(feature = "bench")]
    pub fn selection(&self) -> Vec<(usize, Vec<accent_core::pdf::Rect>)> {
        self.imp().selection.borrow().clone()
    }

    /// Called with the page and the point a drag started and ended on, which need not be the
    /// same page.
    pub fn connect_select(&self, f: impl Fn(&PdfView, Span) + 'static) {
        *self.imp().on_select.borrow_mut() = Some(Box::new(f));
    }

    /// The layout as one line: what `ACCENT_BENCH_PDF` prints, and its only reader. Whether the
    /// page being read is wholly on screen is what Fit Height has to mean, so that is the last
    /// field rather than something the numbers have to be read for.
    #[cfg(feature = "bench")]
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

    /// The tiles and stand-ins the last frame painted without, as `page` or `page:tx,ty`.
    #[cfg(feature = "bench")]
    pub fn unrendered(&self) -> Vec<String> {
        let wants = self.imp().unrendered.borrow();
        wants
            .iter()
            .map(|w| match w.is_lowres() {
                true => w.page.to_string(),
                false => format!("{}:{},{}", w.page, w.tx, w.ty),
            })
            .collect()
    }

    /// How many tiles on screen the last frame painted blurred or not at all.
    #[cfg(feature = "bench")]
    pub fn unsharp(&self) -> usize {
        self.imp().unsharp.get()
    }
}

mod imp {
    use super::*;
    use crate::scrollable::Adjustments;
    use std::cell::OnceCell;

    /// The view scrolled: paint what is under it now, and report the page if that changed.
    fn moved(view: &super::PdfView) {
        view.queue_draw();
        view.imp().notice_page();
    }

    type Wants = Box<dyn Fn(&super::PdfView, f32, bool, Vec<Want>)>;
    type Coords = Box<dyn Fn(&super::PdfView, f64, f64)>;
    type Click = Box<dyn Fn(&super::PdfView, f64, f64, gdk::ModifierType)>;
    type Page = Box<dyn Fn(usize)>;
    type Zoomed = Box<dyn Fn()>;
    type OnReply = Box<dyn Fn(&super::PdfView, Reply)>;
    type OnSelect = Box<dyn Fn(&super::PdfView, super::Span)>;
    type Lowres = Box<dyn Fn(u32)>;
    type Stroke = Box<dyn Fn(usize, Vec<(f32, f32)>)>;
    /// A page and the id of a stroke on it the eraser passed over, what a partial eraser left of
    /// it, and whether the same drag took one before it.
    type At = Box<dyn Fn(usize, u32, Option<super::Pass>, bool)>;
    type Transform = Box<dyn Fn(usize, u32, accent_core::pdf::Matrix)>;

    #[derive(glib::Properties)]
    #[properties(wrapper_type = super::PdfView)]
    pub struct PdfView {
        // The four `GtkScrollable` properties. GTK reads and writes them by name, so they have to
        // be real GObject properties rather than plain fields.
        #[property(name = "hadjustment", type = Option<gtk::Adjustment>, get = |v: &Self| v.scroll.h(), set = Self::adopt_h, nullable, override_interface = gtk::Scrollable)]
        #[property(name = "vadjustment", type = Option<gtk::Adjustment>, get = |v: &Self| v.scroll.v(), set = Self::adopt_v, nullable, override_interface = gtk::Scrollable)]
        pub scroll: Adjustments,
        #[property(get, set, override_interface = gtk::Scrollable, builder(gtk::ScrollablePolicy::Minimum))]
        pub hscroll_policy: Cell<gtk::ScrollablePolicy>,
        #[property(get, set, override_interface = gtk::Scrollable, builder(gtk::ScrollablePolicy::Minimum))]
        pub vscroll_policy: Cell<gtk::ScrollablePolicy>,
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
        pub stale_pages: RefCell<HashMap<u32, accent_core::pdf::Rect>>,
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
        /// What the eraser and the Adjust tool can find, per page they have been told about.
        pub inks: RefCell<HashMap<usize, Vec<NamedInk>>>,
        pub adjust: RefCell<Option<super::Selected>>,
        /// Where the eraser was last reported in the drag under way, as a page and a point on
        /// it, so the next report tests the line between the two.
        pub erasing: Cell<Option<(usize, (f32, f32))>>,
        /// Whether the drag under way has taken a stroke yet: every later one is part of the
        /// same step for Undo.
        pub erased: Cell<bool>,
        /// How many erases this view has sent, which a list of a page has to have seen to be
        /// current. See [`super::PdfView::set_inks`].
        pub erases: Cell<u64>,
        /// Whether the pointer is the stylus's dot rather than the arrow a tool otherwise shows.
        pub dot: Cell<bool>,
        /// Where the pointer is over the view, until it leaves: what a Ctrl+wheel zooms around.
        pub over: Cell<Option<(f64, f64)>>,
        pub current_mark: Cell<Option<(usize, usize)>>,
        /// What was asked for last, so an unchanged viewport does not re-ask on every frame.
        pub asked: RefCell<Vec<Want>>,
        /// The scale, colour scheme and cache generation [`Self::asked`] was for. Every event that
        /// makes the tiles on screen the wrong ones without changing *which* tiles are wanted
        /// goes through here — a zoom step, a fit mode, a resize, a theme change, entering
        /// presentation mode, a stroke or an erase on a part of the page the last change also
        /// touched — and without it the page keeps painting its old render and never asks again.
        pub asked_for: Cell<(u32, bool, u64)>,
        /// What the last frame found missing, asked for or not: what a drill waits to see empty.
        #[cfg(feature = "bench")]
        pub unrendered: RefCell<Vec<Want>>,
        /// How many tiles on screen the last frame showed other than sharp.
        #[cfg(feature = "bench")]
        pub unsharp: Cell<usize>,
        pub page: Cell<usize>,
        pub shown: RefCell<std::ops::RangeInclusive<usize>>,
        /// The paper colour for the scheme in force, which costs a CSS parse to work out and is
        /// the same for every page of every frame until the theme changes.
        pub paper: Cell<Option<gdk::RGBA>>,
        pub on_wants: RefCell<Option<Wants>>,
        pub on_reply: RefCell<Option<OnReply>>,
        pub on_select: RefCell<Option<OnSelect>>,
        pub on_goto: RefCell<Option<Page>>,
        pub on_pressed: RefCell<Option<Coords>>,
        pub on_clicked: RefCell<Option<Click>>,
        pub on_ink: RefCell<Option<Stroke>>,
        pub on_erase: RefCell<Option<At>>,
        pub on_transform: RefCell<Option<Transform>>,
        pub on_motion: RefCell<Option<Coords>>,
        pub on_page: RefCell<Option<Page>>,
        pub on_shown: RefCell<Option<Box<dyn Fn()>>>,
        pub on_zoom: RefCell<Option<Zoomed>>,
        pub on_lowres: RefCell<Option<Lowres>>,
    }

    // `gtk::ScrollablePolicy` has no `Default`, so the struct spells its own out.
    impl Default for PdfView {
        fn default() -> Self {
            PdfView {
                scroll: Adjustments::default(),
                hscroll_policy: Cell::new(gtk::ScrollablePolicy::Minimum),
                vscroll_policy: Cell::new(gtk::ScrollablePolicy::Minimum),
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
                stale_pages: RefCell::new(HashMap::new()),
                stale_tiles: RefCell::new(HashSet::new()),
                strokes: RefCell::new(Vec::new()),
                inks: RefCell::new(HashMap::new()),
                adjust: RefCell::new(None),
                erasing: Cell::new(None),
                erased: Cell::new(false),
                erases: Cell::new(0),
                dot: Cell::new(false),
                over: Cell::new(None),
                current_mark: Cell::new(None),
                asked: RefCell::new(Vec::new()),
                asked_for: Cell::new((0, false, 0)),
                #[cfg(feature = "bench")]
                unrendered: RefCell::new(Vec::new()),
                #[cfg(feature = "bench")]
                unsharp: Cell::new(0),
                page: Cell::new(0),
                shown: RefCell::new(0..=0),
                paper: Cell::new(None),
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
                on_shown: RefCell::new(None),
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
            self.scroll.adopt(&*self.obj(), 0, adjustment, moved);
        }

        fn adopt_v(&self, adjustment: Option<gtk::Adjustment>) {
            self.scroll.adopt(&*self.obj(), 1, adjustment, moved);
        }

        /// The colour a page's paper is drawn in before its tiles arrive, matching what the
        /// renderer will produce so nothing flashes when they do.
        pub(super) fn paper(&self) -> gdk::RGBA {
            if let Some(colour) = self.paper.get() {
                return colour;
            }
            let colour = super::paint::paper(self.dark.get());
            self.paper.set(Some(colour));
            colour
        }

        /// Report the page being read when it changes, for the header and the thumbnail frame,
        /// and the pages on screen when they do, for the drawing tools. A strip reports nothing:
        /// it frames the page the reading view hands it ([`super::PdfView::set_framed`]), not the
        /// one in the middle of its own viewport.
        fn notice_page(&self) {
            if self.thumbnails.get() {
                return;
            }
            let page = self.obj().current_page();
            if self.page.replace(page) != page {
                self.obj().queue_draw();
                if let Some(f) = self.on_page.borrow().as_ref() {
                    f(page);
                }
            }
            let shown = self.obj().visible_pages();
            if self.shown.replace(shown.clone()) != shown
                && let Some(f) = self.on_shown.borrow().as_ref()
            {
                f();
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

            obj.wire_gestures();
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
            self.paint(snapshot);
        }
    }

    impl ScrollableImpl for PdfView {}
}
