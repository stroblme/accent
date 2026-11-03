//! Scrollable PDF pages painted from cached tiles, without file access or pdfium calls.
//!
//! The widget uses page sizes, zoom and supplied textures. Missing tiles trigger requests
//! and use blurred low-resolution stand-ins; `tab.rs` owns the document and render thread.
//! A reading view and thumbnail view share one document.
//!
//! `paint` draws pages, `gesture` handles the pointer and `scroll` manages reading position.

mod gesture;
mod paint;
mod scroll;

pub(crate) use paint::shown;

use accent_core::pdf::Theme;
use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use super::cache::{Cache, TileKey, Want, texture};
use super::geometry::{Layout, PT_TO_PX, PdfZoom, Span, stepped};
use super::protocol::{Highlights, NamedInk, NamedPicture, NamedText, Pass, Reply};
use super::tools::{Lasso, Mode, Picked, Selection, Stroke};
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

    /// Page sizes in points, sufficient to lay out the document.
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

    /// Paint the pages in `theme`'s colours. The rendering itself recolours, so this changes
    /// which tiles are wanted rather than how they are drawn.
    pub fn set_theme(&self, theme: Theme) {
        if self.imp().theme.replace(theme) != theme {
            self.imp().paper.set(None);
            self.queue_draw();
        }
    }

    pub fn theme(&self) -> Theme {
        self.imp().theme.get()
    }

    /// Take a rendered tile. Ignored if the document has moved on from the scale it was for.
    pub fn insert_tile(&self, key: TileKey, texture: gdk::MemoryTexture, bytes: usize) {
        self.imp().stale_tiles.borrow_mut().remove(&key);
        // A hidden view has let its tiles go, and these were asked for before it was hidden.
        if self.is_mapped() {
            self.cache().borrow_mut().insert(key, texture, bytes);
        }
        // Whatever was drawn on this page is in its pixels now, so the stroke painted over the
        // top can go. Here rather than when the stroke was sent: the render is what replaces it,
        // and dropping it any earlier is a gap the reader sees.
        self.settle_stroke(key.page as usize);
        if let Some(f) = self.imp().on_tile.borrow().as_ref() {
            f(key.page as usize);
        }
        self.queue_draw();
    }

    /// Called with the page of every tile that lands.
    pub fn connect_tile(&self, f: impl Fn(usize) + 'static) {
        *self.imp().on_tile.borrow_mut() = Some(Box::new(f));
    }

    pub fn insert_lowres(&self, page: u32, theme: Theme, texture: gdk::MemoryTexture) {
        self.cache()
            .borrow_mut()
            .insert_lowres(page, theme, texture);
        self.queue_draw();
    }

    /// Called with the tiles this view wants and does not have, visible ones first.
    pub fn connect_wants(&self, f: impl Fn(&PdfView, f32, Theme, Vec<Want>) + 'static) {
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
            Reply::Lowres { page, theme, image } => {
                self.insert_lowres(page, theme, texture(image));
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
        // A palette that is not the palette it was: the paper moved with it.
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

    /// Report primary-click release and modifiers only after ruling out a drag. Highlight
    /// navigation must not fire when selection starts inside one; links retain press-time
    /// handling.
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

    /// The points of every stroke the pen has finished. Only `ACCENT_BENCH_PDF=pen:` reads it.
    #[cfg(feature = "bench")]
    pub fn drawn_points(&self) -> usize {
        self.imp().drawn.get()
    }

    /// How many strokes the render thread has listed for the tools, and the list held of `page`.
    /// Only `ACCENT_BENCH_PDF=pen:` reads them.
    #[cfg(feature = "bench")]
    pub fn inks_heard(&self, page: usize) -> (usize, Option<Vec<NamedInk>>) {
        let held = self.imp().inks.borrow().get(&page).cloned();
        (self.imp().heard.get(), held)
    }

    /// The texts of ours the view holds of `page`. Only `ACCENT_BENCH_PDF=text:` asks.
    #[cfg(feature = "bench")]
    pub fn texts_heard(&self, page: usize) -> Option<Vec<NamedText>> {
        self.imp().texts.borrow().get(&page).cloned()
    }

    /// The page and box of what the Select tool holds, and how many items it is. Only drills
    /// ask.
    #[cfg(feature = "bench")]
    pub fn picked_box(&self) -> Option<(usize, accent_core::pdf::Rect, usize)> {
        let picked = self.imp().picked.borrow();
        picked.as_ref().map(|s| (s.page, s.bounds(), s.items.len()))
    }

    /// How many lists of a page the render thread has sent the tools. Only
    /// `ACCENT_BENCH_PDF=pen:` reads it.
    #[cfg(feature = "bench")]
    pub fn lists_heard(&self) -> usize {
        self.imp().lists.get()
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
        if mode != Mode::Select {
            self.clear_inks();
            self.imp().lasso.replace(None);
        }
        // The plain pointer for every tool, and never the I-beam the page otherwise shows: with a
        // tool in hand a drag draws rather than selects, and a cursor that says "text" invites
        // exactly the thing that will not happen — but under the Text tool, where a click types.
        // A stylus's next move puts its dot back.
        self.imp().dot.set(false);
        self.set_cursor_from_name(match mode {
            Mode::Read => None,
            Mode::Text => Some("text"),
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
            width: crate::pdf::tools::width_of(mode.style_owner(), width),
            rgba: [r, g, b, (alpha * 255.0) as u8],
            multiply,
        }
    }

    /// How the Text tool sets a new text, from the preferences: the nearest of its three sizes,
    /// and its colour, the accent where none was picked.
    pub fn text_style(&self) -> accent_core::pdf::TextStyle {
        let c = self.imp().style.borrow();
        accent_core::pdf::TextStyle {
            size: crate::pdf::tools::width_of(Mode::Text, c.text_size),
            rgb: c.text_color.unwrap_or_else(theme::accent_rgb),
        }
    }

    /// Called with where the Text tool pressed, a page and a point on it, and the text of ours
    /// under it, if there is one.
    pub fn connect_type(&self, f: impl Fn(usize, (f32, f32), Option<NamedText>) + 'static) {
        *self.imp().on_type.borrow_mut() = Some(Box::new(f));
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

    /// Called when an eraser drag that reached a page ends.
    pub fn connect_erased(&self, f: impl Fn() + 'static) {
        *self.imp().on_erased.borrow_mut() = Some(Box::new(f));
    }

    /// Whether an eraser drag is under way over the pages: it goes on with the strokes the view
    /// holds, and needs none read meanwhile.
    pub fn erasing(&self) -> bool {
        self.imp().erasing.get().is_some()
    }

    /// Called with whether the pen's eraser end came to the page, or another hand after it.
    pub fn connect_eraser(&self, f: impl Fn(bool) + 'static) {
        *self.imp().on_eraser.borrow_mut() = Some(Box::new(f));
    }

    /// Called with a page, the ids of strokes and texts on it and the map to move them by.
    pub fn connect_transform(
        &self,
        f: impl Fn(usize, Vec<u32>, accent_core::pdf::Matrix) + 'static,
    ) {
        *self.imp().on_transform.borrow_mut() = Some(Box::new(f));
    }

    /// Called whenever what the Select tool holds changes.
    pub fn connect_picked(&self, f: impl Fn() + 'static) {
        *self.imp().on_picked.borrow_mut() = Some(Box::new(f));
    }

    /// The page and the ids of what the Select tool holds, if anything.
    pub fn picked(&self) -> Option<(usize, Vec<u32>)> {
        let picked = self.imp().picked.borrow();
        picked.as_ref().map(|s| (s.page, s.ids()))
    }

    /// The box of what the Select tool holds on its page, if anything.
    pub fn picked_bounds(&self) -> Option<(usize, accent_core::pdf::Rect)> {
        let picked = self.imp().picked.borrow();
        picked.as_ref().map(|s| (s.page, s.bounds()))
    }

    /// Hold `items` of `page`, which the render thread is about to draw under their ids: what a
    /// paste leaves picked. The page's list read before they land is of the page without them.
    pub fn pick(&self, page: usize, items: Vec<Picked>) {
        self.relisted(page);
        self.imp().picked.replace(Some(Selection::of(page, items)));
        self.picked_changed();
    }

    /// Let go of what the Select tool holds; whether it held anything.
    pub fn clear_pick(&self) -> bool {
        let held = self.imp().picked.take().is_some();
        if held {
            self.picked_changed();
        }
        held
    }

    /// Take what the Select tool holds off the page's list at once, as an erase does, and say
    /// which page and ids it was: what a delete and a cut send.
    pub fn drop_picked(&self) -> Option<(usize, Vec<u32>)> {
        let selection = self.imp().picked.take()?;
        let (page, ids) = (selection.page, selection.ids());
        if let Some(list) = self.imp().inks.borrow_mut().get_mut(&page) {
            list.retain(|(id, _)| !ids.contains(id));
        }
        if let Some(list) = self.imp().texts.borrow_mut().get_mut(&page) {
            list.retain(|(id, _)| !ids.contains(id));
        }
        if let Some(list) = self.imp().pictures.borrow_mut().get_mut(&page) {
            list.retain(|(id, _)| !ids.contains(id));
        }
        self.relisted(page);
        self.picked_changed();
        Some((page, ids))
    }

    /// Move what the Select tool holds by `dx`, `dy` points, as far as its page allows: the
    /// arrow keys. Whether it held anything.
    pub fn nudge(&self, dx: f32, dy: f32) -> bool {
        let moved = {
            let picked = self.imp().picked.borrow();
            let Some(s) = picked.as_ref() else {
                return false;
            };
            let size = self.page_size(s.page).unwrap_or((f32::MAX, f32::MAX));
            super::tools::clamp_move(s.bounds(), [1.0, 0.0, 0.0, 1.0, dx, dy], size)
        };
        self.transform_picked(moved);
        true
    }

    /// Move what the Select tool holds by `m` and send it to the tab. Its items, and the page's
    /// lists, go where the render thread will put them, so an eraser pass right after the move
    /// finds them there.
    fn transform_picked(&self, m: accent_core::pdf::Matrix) {
        let (page, items) = {
            let mut picked = self.imp().picked.borrow_mut();
            let Some(s) = picked.as_mut() else {
                return;
            };
            s.items = s.items.iter().map(|item| item.under(m)).collect();
            (s.page, s.items.clone())
        };
        let moved = |id: &u32| items.iter().find(|item| item.id == *id);
        let mut inks = self.imp().inks.borrow_mut();
        for (id, ink) in inks.get_mut(&page).into_iter().flatten() {
            if let Some(item) = moved(id) {
                ink.points = item.points.clone();
                ink.bounds = item.bounds;
            }
        }
        let mut texts = self.imp().texts.borrow_mut();
        for (id, text) in texts.get_mut(&page).into_iter().flatten() {
            if let Some(item) = moved(id) {
                text.bounds = item.bounds;
                text.typed.at = (item.bounds.left, item.bounds.top);
            }
        }
        let mut pictures = self.imp().pictures.borrow_mut();
        for (id, picture) in pictures.get_mut(&page).into_iter().flatten() {
            if let Some(item) = moved(id) {
                picture.place = accent_core::pdf::then(picture.place, m);
                picture.bounds = item.bounds;
            }
        }
        drop((inks, texts, pictures));
        self.relisted(page);
        let ids = items.iter().map(|item| item.id).collect();
        if let Some(f) = self.imp().on_transform.borrow().as_ref() {
            f(page, ids, m);
        }
        self.queue_draw();
    }

    /// Count a change this view makes to `page`'s list as it sends it, which a list of the page
    /// has to have seen to be current ([`PdfView::set_inks`]): an erase, a move, a delete, a cut
    /// or a paste. The render thread counts the same requests (`render::relists`).
    fn relisted(&self, page: usize) {
        let sent = self.imp().erases.get() + 1;
        self.imp().erases.set(sent);
        self.imp().last_erase.borrow_mut().insert(page, sent);
    }

    /// Repaint, and tell the tab, whose ring and menu offer what can be done with it.
    fn picked_changed(&self) {
        self.queue_draw();
        if let Some(f) = self.imp().on_picked.borrow().as_ref() {
            f();
        }
    }

    /// Apply page strokes only if their `erases` count includes this view's latest erase on the
    /// page. Older replies are stale relative to locally updated strokes; the erase sends a fresh
    /// list. An erase on another page does not hold this one's back: nothing would ask again.
    /// What the Select tool holds follows its strokes and texts by their ids, letting go of one
    /// no longer on the page.
    pub fn set_inks(
        &self,
        page: usize,
        inks: Vec<NamedInk>,
        texts: Vec<NamedText>,
        pictures: Vec<NamedPicture>,
        erases: u64,
    ) {
        #[cfg(feature = "bench")]
        {
            self.imp().heard.set(self.imp().heard.get() + inks.len());
            self.imp().lists.set(self.imp().lists.get() + 1);
        }
        let latest = self.imp().last_erase.borrow().get(&page).copied();
        if erases < latest.unwrap_or(0) {
            return;
        }
        let followed = {
            let mut picked = self.imp().picked.borrow_mut();
            match picked.as_mut().filter(|s| s.page == page) {
                Some(s) => {
                    let was = s.items.len();
                    s.items = (s.items.iter())
                        .filter_map(|item| {
                            let stroke = inks.iter().find(|(id, _)| *id == item.id);
                            let text = || texts.iter().find(|(id, _)| *id == item.id);
                            let picture = || pictures.iter().find(|(id, _)| *id == item.id);
                            (stroke.map(Picked::stroke))
                                .or_else(|| text().map(Picked::text))
                                .or_else(|| picture().map(Picked::picture))
                        })
                        .collect();
                    let lost = s.items.len() != was;
                    if s.items.is_empty() {
                        *picked = None;
                    }
                    lost
                }
                None => false,
            }
        };
        if followed {
            self.picked_changed();
        }
        self.imp().inks.borrow_mut().insert(page, inks);
        self.imp().texts.borrow_mut().insert(page, texts);
        self.imp().pictures.borrow_mut().insert(page, pictures);
        self.queue_draw();
    }

    /// A stroke just drawn joins its page's list, where one is held: a page without one is read
    /// whole when the tools ask for it.
    pub fn add_ink(&self, page: usize, ink: NamedInk) {
        #[cfg(feature = "bench")]
        self.imp().heard.set(self.imp().heard.get() + 1);
        if let Some(list) = self.imp().inks.borrow_mut().get_mut(&page) {
            list.push(ink);
        }
    }

    /// Forget every stroke, text and picture the tools knew: the document changed under them.
    pub fn clear_inks(&self) {
        self.imp().inks.borrow_mut().clear();
        self.imp().texts.borrow_mut().clear();
        self.imp().pictures.borrow_mut().clear();
        self.clear_pick();
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

    /// Refresh changed page tiles while retaining the visible render until replacements arrive,
    /// avoiding blank flashes after strokes, erases or export.
    pub fn refresh_page(&self, page: usize, area: accent_core::pdf::Rect) {
        let (scale_milli, theme) = self.stamp();
        self.cache()
            .borrow_mut()
            .forget_page_except(page as u32, scale_milli, theme);
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
    fn stamp(&self) -> (u32, Theme) {
        let scale = self.imp().layout.borrow().scale * self.scale_factor().max(1) as f32;
        ((scale * 1000.0).round() as u32, self.imp().theme.get())
    }

    /// The one match to draw more strongly than the rest.
    pub fn set_current_mark(&self, at: Option<(usize, usize)>) {
        self.imp().current_mark.set(at);
        self.queue_draw();
    }

    /// Draw the line in the gap a dropped PDF's pages would go into, or none.
    pub fn set_drop_gap(&self, gap: Option<usize>) {
        if self.imp().drop_gap.replace(gap) != gap {
            self.queue_draw();
        }
    }

    #[cfg(feature = "bench")]
    pub fn drop_gap(&self) -> Option<usize> {
        self.imp().drop_gap.get()
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

    type Wants = Box<dyn Fn(&super::PdfView, f32, Theme, Vec<Want>)>;
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
    type Transform = Box<dyn Fn(usize, Vec<u32>, accent_core::pdf::Matrix)>;
    /// Where the Text tool pressed, and the text of ours there.
    type Type = Box<dyn Fn(usize, (f32, f32), Option<NamedText>)>;
    /// Whether the pen's eraser end came to the page, or another hand after it.
    type EraserEnd = Box<dyn Fn(bool)>;
    type Jump = Box<dyn FnOnce(&super::PdfView)>;

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
        pub theme: Cell<Theme>,
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
        /// is attached, or Eraser for the stylus's eraser tip — decided once, on the press;
        /// `None` for a finger's, which pans (`tools::drag_of`).
        pub drag_mode: Cell<Option<super::Mode>>,
        /// Whether the finger on the page was held still for a long press, which makes its drag
        /// a selection rather than a pan.
        pub held: Cell<bool>,
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
        /// What the eraser and the Select tool can find, per page they have been told about, and
        /// the texts of ours, which the Text tool types again too, and the pictures of ours,
        /// which only the Select tool takes hold of.
        pub inks: RefCell<HashMap<usize, Vec<NamedInk>>>,
        pub texts: RefCell<HashMap<usize, Vec<NamedText>>>,
        pub pictures: RefCell<HashMap<usize, Vec<NamedPicture>>>,
        /// What the Select tool holds, and the lasso it is drawing.
        pub picked: RefCell<Option<super::Selection>>,
        pub lasso: RefCell<Option<super::Lasso>>,
        /// Where the eraser was last reported in the drag under way, as a page and a point on
        /// it, so the next report tests the line between the two; `None` once the drag ends.
        pub erasing: Cell<Option<(usize, (f32, f32))>>,
        /// Whether the drag under way has taken a stroke yet: every later one is part of the
        /// same step for Undo.
        pub erased: Cell<bool>,
        /// How many erases this view has sent.
        pub erases: Cell<u64>,
        /// How many it had sent with the latest on each page, which a list of that page has to
        /// have seen to be current. See [`super::PdfView::set_inks`].
        pub last_erase: RefCell<HashMap<usize, u64>>,
        /// Whether the pointer is the stylus's dot rather than the arrow a tool otherwise shows.
        pub dot: Cell<bool>,
        /// Whether the last hand over the page was the pen's eraser end.
        pub eraser: Cell<bool>,
        /// Where the pointer is over the view, until it leaves: what a Ctrl+wheel zooms around.
        pub over: Cell<Option<(f64, f64)>>,
        pub current_mark: Cell<Option<(usize, usize)>>,
        /// A jump asked for before the first layout, made by it. See
        /// [`super::PdfView::after_layout`].
        pub jump: RefCell<Option<Jump>>,
        /// The gap between pages a PDF dragged over the view would go into, where a line shows.
        pub drop_gap: Cell<Option<usize>>,
        /// What was asked for last, so an unchanged viewport does not re-ask on every frame.
        pub asked: RefCell<Vec<Want>>,
        /// Scale, theme and cache generation of [`Self::asked`]. Re-request unchanged tile keys
        /// when their content changes, including after zoom, resize, theme or annotation
        /// changes.
        pub asked_for: Cell<(u32, Theme, u64)>,
        /// What the last frame found missing, asked for or not: what a drill waits to see empty.
        #[cfg(feature = "bench")]
        pub unrendered: RefCell<Vec<Want>>,
        /// How many tiles on screen the last frame showed other than sharp.
        #[cfg(feature = "bench")]
        pub unsharp: Cell<usize>,
        /// The points of every stroke the pen has finished, as the gesture took them.
        #[cfg(feature = "bench")]
        pub drawn: Cell<usize>,
        /// How many strokes the render thread has listed for the tools, every list counted whole.
        #[cfg(feature = "bench")]
        pub heard: Cell<usize>,
        /// How many lists of a page it has sent them, stale ones included.
        #[cfg(feature = "bench")]
        pub lists: Cell<usize>,
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
        pub on_erased: RefCell<Option<Box<dyn Fn()>>>,
        pub on_transform: RefCell<Option<Transform>>,
        pub on_picked: RefCell<Option<Box<dyn Fn()>>>,
        pub on_type: RefCell<Option<Type>>,
        pub on_tile: RefCell<Option<Page>>,
        pub on_eraser: RefCell<Option<EraserEnd>>,
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
                theme: Cell::new(Theme::Plain),
                thumbnails: Cell::new(false),
                marks: RefCell::new(HashMap::new()),
                highlights: RefCell::new(HashMap::new()),
                selection: RefCell::new(Vec::new()),
                drag_from: Cell::new(None),
                mode: Cell::new(super::Mode::default()),
                drag_mode: Cell::new(None),
                held: Cell::new(false),
                style: RefCell::new(accent_core::config::DrawingConfig::default()),
                stale_pages: RefCell::new(HashMap::new()),
                stale_tiles: RefCell::new(HashSet::new()),
                strokes: RefCell::new(Vec::new()),
                inks: RefCell::new(HashMap::new()),
                texts: RefCell::new(HashMap::new()),
                pictures: RefCell::new(HashMap::new()),
                picked: RefCell::new(None),
                lasso: RefCell::new(None),
                erasing: Cell::new(None),
                erased: Cell::new(false),
                erases: Cell::new(0),
                last_erase: RefCell::new(HashMap::new()),
                dot: Cell::new(false),
                eraser: Cell::new(false),
                over: Cell::new(None),
                current_mark: Cell::new(None),
                jump: RefCell::new(None),
                drop_gap: Cell::new(None),
                asked: RefCell::new(Vec::new()),
                asked_for: Cell::new((0, Theme::Plain, 0)),
                #[cfg(feature = "bench")]
                unrendered: RefCell::new(Vec::new()),
                #[cfg(feature = "bench")]
                unsharp: Cell::new(0),
                #[cfg(feature = "bench")]
                drawn: Cell::new(0),
                #[cfg(feature = "bench")]
                heard: Cell::new(0),
                #[cfg(feature = "bench")]
                lists: Cell::new(0),
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
                on_erased: RefCell::new(None),
                on_transform: RefCell::new(None),
                on_picked: RefCell::new(None),
                on_type: RefCell::new(None),
                on_tile: RefCell::new(None),
                on_eraser: RefCell::new(None),
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
            let colour = super::paint::paper(self.theme.get());
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

        /// A tab switched away from lets go of its tiles, up to [`Cache`]'s 256 MB, and keeps
        /// the stand-ins: coming back paints those and asks again, about 100 ms of blur.
        fn unmap(&self) {
            self.parent_unmap();
            if !self.thumbnails.get() {
                self.obj().cache().borrow_mut().drop_tiles();
                // Or the same list asked for again would look already asked for.
                self.asked.borrow_mut().clear();
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.paint(snapshot);
        }
    }

    impl ScrollableImpl for PdfView {}
}
