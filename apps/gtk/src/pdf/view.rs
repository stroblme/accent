//! The widget a PDF is read in: a scrollable column of pages painted from cached tiles.
//!
//! Nothing here opens a file or calls pdfium. The widget knows the page sizes, the zoom and the
//! textures it has been given; when it paints a page it has no tiles for, it asks for them and
//! draws a blurred low-resolution stand-in until they arrive. `tab.rs` owns the document and the
//! thread that answers.
//!
//! Two of these share one document: the reading view, and a narrow one beside it showing every
//! page as a thumbnail.

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use super::cache::{Cache, TILE, TileKey, Want, texture, tiles_across};
use super::geometry::{
    Anchor, Layout, PT_TO_PX, PdfZoom, Span, anchor_at, clamp_scale, fit_scale, layout, offset_of,
    page_at, resume_at, stepped,
};
use super::protocol::{Highlights, Reply};
use super::tools::{
    ADJUST_RADIUS, HANDLE, HIGHLIGHTER_ALPHA, Handle, Mode, Selected, Stroke, drag_matrix,
    handle_at, mapped, shape_of, snap,
};

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
        let (width, colour, alpha, multiply) = match mode.style_owner() {
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

    /// The tool a press really means. A pen draws, its eraser tip erases whatever is in hand,
    /// and a finger never draws — touch is for moving the page. A mouse draws while nothing
    /// but a mouse is plugged in; with a pen attached the hand selects, unless the preference
    /// says otherwise.
    fn effective_mode(&self, controller: &impl IsA<gtk::EventController>) -> Mode {
        let mode = self.imp().mode.get();
        if mode == Mode::Select {
            return mode;
        }
        // A pen is known by its tool: on Wayland a tablet's events arrive on a logical device
        // whose source is a mouse, and only the tool says otherwise. X11 without libwacom has
        // no tool, and there the device's source is what says pen.
        let tool = controller.current_event().and_then(|e| e.device_tool());
        if tool
            .as_ref()
            .is_some_and(|t| t.tool_type() == gdk::DeviceToolType::Eraser)
        {
            return Mode::Eraser;
        }
        let source = controller.current_event_device().map(|d| d.source());
        if tool.is_some() || source == Some(gdk::InputSource::Pen) {
            return mode;
        }
        if source == Some(gdk::InputSource::Touchscreen) {
            return Mode::Select;
        }
        match self.imp().style.borrow().mouse || !stylus_attached() {
            true => mode,
            false => Mode::Select,
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
        let at = layout.to_page(page, cx, cy);
        let Some(((x, y), (pw, ph))) = at.zip(self.page_size(page)) else {
            return (0.0, 0.0);
        };
        (x.clamp(0.0, pw), y.clamp(0.0, ph))
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
        let (page, _) = layout
            .pages
            .iter()
            .enumerate()
            .find(|(_, r)| cy >= r.y && cy <= r.y + r.h && cx >= r.x && cx <= r.x + r.w)?;
        let (x, y) = layout.to_page(page, cx, cy)?;
        Some((page, x, y))
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
        Some((page, layout.to_page(page, cx, cy)?))
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
                    let steps = crate::zoom::wheel_steps(&obj.imp().scroll_accum, dy);
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
                        snapshot.append_color(&colour, &layout.rect_of(rect, quad));
                    }
                }
                if let Some((_, boxes)) = selection.iter().find(|(at, _)| *at == index) {
                    let colour = gdk::RGBA::new(accent.red(), accent.green(), accent.blue(), 0.35);
                    for glyph in boxes {
                        snapshot.append_color(&colour, &layout.rect_of(rect, glyph));
                    }
                }
                for stroke in strokes.iter().filter(|s| s.page == index) {
                    let builder = gsk::PathBuilder::new();
                    let point = |&(x, y): &(f32, f32)| {
                        graphene::Point::new(rect.x + x * layout.scale, rect.y + y * layout.scale)
                    };
                    let points = &stroke.points;
                    match (stroke.tool, points.as_slice()) {
                        (Mode::Rect, &[a, b]) => {
                            builder.add_rect(
                                &layout.rect_of(rect, &accent_core::pdf::Rect::from_corners(a, b)),
                            );
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
                    let moved = mapped(a.bounds, a.matrix);
                    let frame = layout.rect_of(rect, &moved);
                    let (l, t) = (frame.x(), frame.y());
                    let (r, b) = (l + frame.width(), t + frame.height());
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
                        snapshot.append_color(&colour, &layout.rect_of(rect, mark));
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
