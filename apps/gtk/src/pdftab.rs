//! A PDF in a tab: the document, the thread that renders it, and the reading state around it.
//!
//! The widget in `pdfview.rs` knows nothing about pdfium; it asks for tiles and paints what it is
//! given. This is the other half: one thread per open document, which owns the `PdfDoc` and
//! answers those requests, plus the history, the search and the outline that make it a reader
//! rather than a viewer.
//!
//! pdfium is serialised behind one process-wide lock (see `accent_core::pdf`), so one thread per
//! document is not a limitation we could lift by adding more.

use crate::pdfview::{self, Anchor, PdfView, PdfZoom, Reply, TileKey, Want};
use accent_core::pdf::{self, LinkTarget, PdfDoc};
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{Sender, TryRecvError, channel};

/// How many places back the reader can go. A reading history is not an undo stack; a hundred is
/// far past what anyone follows in one sitting.
const HISTORY: usize = 100;

/// How tall a link preview's band is, in the low-resolution page's own pixels. Roughly a quarter
/// of a portrait page at [`pdfview::LOWRES_W`], which is a heading and the lines under it: a
/// whole page shrunk to a popover says nothing a reader can read.
const BAND: i32 = 96;

/// What the render thread is asked for.
enum Request {
    /// Visible tiles first, then one viewport of prefetch. A newer batch replaces an older one.
    ///
    /// The colours are resolved by the caller, not here: `theme.rs` keeps the chosen theme in
    /// thread-local state, so a render thread asking it would always get the default.
    Tiles {
        scale: f32,
        dark: bool,
        theme: pdf::Theme,
        wants: Vec<Want>,
    },
    Links(usize),
    /// The glyphs of one page, so text on it can be selected.
    Text(usize),
    Outline,
    Search {
        query: u64,
        text: String,
    },
    Reload,
}

/// Where a document is being read, remembered per file in the session.
pub use accent_core::config::PdfPlace as Place;

type Hook = RefCell<Option<Rc<dyn Fn(&Rc<PdfTab>)>>>;
type UriHook = RefCell<Option<Rc<dyn Fn(&str)>>>;

/// A drag over one page, as the two page points it ran between.
type Drag = (usize, (f32, f32), (f32, f32));

/// The Ctrl-hover link preview currently on screen.
struct Preview {
    popover: gtk::Popover,
    /// Where the band of the target page goes. Empty until the render lands, which for a page
    /// nobody has looked at yet is a moment after the popover is up.
    band: adw::Bin,
    /// What it is showing, so a pointer still on the same link asks for nothing again.
    target: LinkTarget,
}

pub struct PdfTab {
    key: RefCell<String>,
    path: RefCell<PathBuf>,
    pub page: adw::TabPage,
    /// "view" once a document is open, "status" when there is nothing to show and a reason why.
    stack: gtk::Stack,
    status: adw::StatusPage,
    view: PdfView,
    thumbs: PdfView,
    /// A plain box between the stack and the scroller, and the link preview's parent: a popover
    /// hung off a widget with a `size_allocate` of its own never re-presents (DESIGN.md, States),
    /// and `PdfView` has one.
    host: gtk::Box,
    /// The strip the thumbnails live in, built once. Handing the Outline pane a fresh
    /// `GtkScrolledWindow` around the same widget every time would re-parent a widget that
    /// already has a parent, which GTK refuses with a critical.
    thumb_strip: gtk::ScrolledWindow,
    /// Requests to the render thread. Dropping it is what ends the thread, so it is dropped with
    /// the tab and nothing else has to be joined.
    tx: RefCell<Option<Sender<Request>>>,
    /// Reading positions behind and ahead, for the mouse's back button and Alt+Left.
    history: RefCell<Vec<Anchor>>,
    future: RefCell<Vec<Anchor>>,
    /// Colours inverted against the system's choice, for a document that renders badly either way.
    inverted: Cell<bool>,
    /// The document could not be opened, so it is not opening either.
    failed: Cell<bool>,
    /// The zoom to restore when presentation mode ends.
    presenting: Cell<Option<PdfZoom>>,
    links: RefCell<std::collections::HashMap<usize, Vec<pdf::Link>>>,
    /// Each page's glyphs, fetched the first time someone drags across that page.
    glyphs: RefCell<std::collections::HashMap<usize, Vec<pdf::Glyph>>>,
    /// The selected text, for Ctrl+C.
    selected: RefCell<String>,
    outline: RefCell<Vec<pdf::Outline>>,
    /// The link preview on screen, if the pointer is on a link with Ctrl held.
    preview: RefCell<Option<Preview>>,
    /// A drag that arrived before the page's glyphs did, to answer when they land.
    pending_select: Cell<Option<Drag>>,
    /// Where the session says this document was left, until the first page sizes arrive and it
    /// can be applied. `None` afterwards, so a reload keeps the reader where they are instead.
    pending: Cell<Option<Place>>,
    /// Which search these results belong to, so a stale page's answer is dropped.
    query: Cell<u64>,
    matches: RefCell<Vec<(usize, pdf::Rect)>>,
    current: Cell<Option<usize>>,
    on_zoom: Hook,
    on_page: Hook,
    on_outline: Hook,
    /// Fired when the document's pages are known, which is when it stops being "opening".
    on_open: Hook,
    on_matches: Hook,
    on_uri: UriHook,
}

/// Open `path` in a new tab of `tabs`. Never fails: a document that will not open is a tab
/// holding the reason.
pub fn open(
    path: &Path,
    key: &str,
    title: &str,
    tooltip: &str,
    tabs: &adw::TabView,
    place: Place,
) -> Rc<PdfTab> {
    let view = PdfView::new();
    let thumbs = PdfView::thumbnails();
    // One cache and one render thread for both views: a thumbnail is the same page at another
    // scale, and rendering it twice would be work for nothing.
    thumbs.set_cache(view.cache());

    let scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&view)
        .build();
    let status = adw::StatusPage::builder()
        .icon_name("x-office-document-symbolic")
        .build();
    let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
    host.append(&scroller);
    let stack = gtk::Stack::new();
    stack.add_named(&host, Some("view"));
    stack.add_named(&status, Some("status"));

    let page = tabs.append(&stack);
    page.set_title(title);
    page.set_tooltip(tooltip);
    page.set_icon(Some(&gio::ThemedIcon::new("x-office-document-symbolic")));

    let tab = Rc::new(PdfTab {
        key: RefCell::new(key.to_string()),
        path: RefCell::new(path.to_path_buf()),
        page,
        stack,
        status,
        view: view.clone(),
        thumbs: thumbs.clone(),
        host,
        thumb_strip: gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&thumbs)
            .build(),
        tx: RefCell::new(None),
        history: RefCell::new(Vec::new()),
        future: RefCell::new(Vec::new()),
        inverted: Cell::new(false),
        failed: Cell::new(false),
        presenting: Cell::new(None),
        pending: Cell::new(Some(place)),
        pending_select: Cell::new(None),
        links: RefCell::new(std::collections::HashMap::new()),
        glyphs: RefCell::new(std::collections::HashMap::new()),
        selected: RefCell::new(String::new()),
        outline: RefCell::new(Vec::new()),
        preview: RefCell::new(None),
        query: Cell::new(0),
        matches: RefCell::new(Vec::new()),
        current: Cell::new(None),
        on_zoom: RefCell::new(None),
        on_page: RefCell::new(None),
        on_outline: RefCell::new(None),
        on_open: RefCell::new(None),
        on_matches: RefCell::new(None),
        on_uri: RefCell::new(None),
    });

    tab.view.set_zoom(place.zoom);
    tab.restyle();
    tab.wire(&view);
    // Only the reading view: the strip is fitted to its own width and never changes zoom.
    view.connect_zoom(glib::clone!(
        #[weak]
        tab,
        move || tab.emit(&tab.on_zoom)
    ));
    tab.wire(&thumbs);
    tab.wire_keys();
    tab.wire_preview();

    // Nothing about the document is known yet, and deliberately so: opening it and measuring its
    // pages is pdfium work, which for a thousand-page file is most of a second. The tab goes up
    // empty and fills in when the render thread reports back, so the window is on screen in the
    // time it takes to build a widget.
    tab.stack.set_visible_child_name("view");
    if let Err(message) = tab.start() {
        tab.show_status(&message);
    }
    tab
}

impl PdfTab {
    pub fn key(&self) -> String {
        self.key.borrow().clone()
    }

    pub fn path(&self) -> PathBuf {
        self.path.borrow().clone()
    }

    pub fn page_count(&self) -> usize {
        self.view.page_count()
    }

    pub fn place(&self) -> Place {
        Place {
            page: self.view.current_page(),
            zoom: self.view.zoom(),
        }
    }

    /// A rename landed: follow the file without losing where the reader is.
    pub fn retarget(&self, root: &Path, key: &str) {
        *self.key.borrow_mut() = key.to_string();
        *self.path.borrow_mut() = root.join(key);
        self.page.set_title(crate::doc::file_name(key));
        self.page
            .set_tooltip(&crate::fileops::display_path(root, key));
    }

    /// Follow the system's light/dark choice, unless this document has been inverted by hand.
    pub fn restyle(&self) {
        let dark = adw::StyleManager::default().is_dark() != self.inverted.get();
        self.view.set_dark(dark);
        self.thumbs.set_dark(dark);
    }

    pub fn toggle_invert(&self) {
        self.inverted.set(!self.inverted.get());
        self.restyle();
    }

    pub fn set_zoom(self: &Rc<Self>, zoom: PdfZoom) {
        self.view.set_zoom(zoom);
    }

    pub fn zoom_step(self: &Rc<Self>, out: bool) {
        self.view.zoom_step(out, None);
    }

    /// The status bar's readout: what the pages are fitted to, or the percentage they are at.
    pub fn zoom_label(&self) -> Option<String> {
        pdfview::zoom_label(self.view.zoom())
    }

    /// Presentation mode shows one whole page and puts the zoom back on the way out.
    pub fn set_presenting(self: &Rc<Self>, on: bool) {
        match (on, self.presenting.get()) {
            (true, None) => {
                self.presenting.set(Some(self.view.zoom()));
                self.set_zoom(PdfZoom::FitPage);
            }
            (false, Some(before)) => {
                self.presenting.set(None);
                self.set_zoom(before);
            }
            _ => {}
        }
    }

    /// Go to a page, remembering where the reader was so Back returns there.
    pub fn goto_page(self: &Rc<Self>, page: usize) {
        self.push_history();
        self.view.goto_page(page, None);
    }

    /// Show a page without remembering where the reader was. Paging is reading and a jump is a
    /// jump: a history full of single steps has nothing left to go back to.
    fn show_page(&self, page: usize) {
        self.view.goto_page(page, None);
    }

    pub fn next_page(&self) {
        self.show_page((self.view.current_page() + 1).min(self.page_count().saturating_sub(1)));
    }

    pub fn previous_page(&self) {
        self.show_page(self.view.current_page().saturating_sub(1));
    }

    pub fn back(self: &Rc<Self>) {
        let Some(to) = self.history.borrow_mut().pop() else {
            return;
        };
        self.future.borrow_mut().push(self.view.anchor());
        self.view.scroll_to(to);
    }

    pub fn forward(self: &Rc<Self>) {
        let Some(to) = self.future.borrow_mut().pop() else {
            return;
        };
        self.history.borrow_mut().push(self.view.anchor());
        self.view.scroll_to(to);
    }

    /// Put the selected text on the clipboard. Nothing selected is not an error: Ctrl+C on a
    /// page with no selection simply leaves the clipboard alone.
    pub fn copy_selection(&self) {
        let text = self.selected.borrow().clone();
        if text.is_empty() {
            return;
        }
        self.view.clipboard().set_text(&text);
    }

    /// The bookmarks, for the Outline pane.
    pub fn outline(&self) -> Vec<pdf::Outline> {
        self.outline.borrow().clone()
    }

    /// The thumbnail strip, for the Outline pane to show under the bookmarks.
    ///
    /// Built once and handed out again on every refresh, so it keeps its scroll position and its
    /// textures. It takes itself out of whatever held it last: the pane rebuilds its container
    /// each time, and a widget with two parents is a GTK critical.
    pub fn thumbnails(&self) -> gtk::Widget {
        if let Some(parent) = self.thumb_strip.parent() {
            match parent.downcast_ref::<gtk::Paned>() {
                Some(paned) => paned.set_end_child(gtk::Widget::NONE),
                None => self.thumb_strip.unparent(),
            }
        }
        self.thumb_strip.clone().upcast()
    }

    /// Search the whole document. An empty query clears what is shown.
    pub fn find(self: &Rc<Self>, text: &str) {
        self.matches.borrow_mut().clear();
        self.current.set(None);
        self.view.set_marks(std::collections::HashMap::new());
        self.query.set(self.query.get() + 1);
        if !text.is_empty() {
            self.ask(Request::Search {
                query: self.query.get(),
                text: text.to_string(),
            });
        }
        self.emit(&self.on_matches);
    }

    /// Step to the next or previous match and scroll it into view.
    pub fn step_match(self: &Rc<Self>, forward: bool) {
        let total = self.matches.borrow().len();
        if total == 0 {
            return;
        }
        let next = match (self.current.get(), forward) {
            (None, true) => 0,
            (None, false) => total - 1,
            (Some(at), true) => (at + 1) % total,
            (Some(at), false) => (at + total - 1) % total,
        };
        self.current.set(Some(next));
        let (page, rect) = self.matches.borrow()[next];
        self.view
            .set_current_mark(Some((page, self.index_on_page(next))));
        self.view.reveal(page, rect);
        self.emit(&self.on_matches);
    }

    /// "3 of 12", or what to say when there is nothing.
    pub fn matches_label(&self) -> String {
        let total = self.matches.borrow().len();
        match (total, self.current.get()) {
            (0, _) => "No results".to_string(),
            (total, None) => format!("{total} matches"),
            (total, Some(at)) => format!("{} of {total}", at + 1),
        }
    }

    /// Where the current match sits among the ones on its own page, which is how the widget
    /// addresses them.
    fn index_on_page(&self, at: usize) -> usize {
        let matches = self.matches.borrow();
        let page = matches[at].0;
        matches[..at].iter().filter(|(p, _)| *p == page).count()
    }

    /// Re-read the file, keeping the page, the scroll and the zoom. A rebuilt PDF is the reason
    /// this exists: a LaTeX loop should not send the reader back to page one.
    pub fn refresh(self: &Rc<Self>) {
        self.ask(Request::Reload);
    }

    pub fn connect_zoom(self: &Rc<Self>, f: impl Fn(&Rc<PdfTab>) + 'static) {
        *self.on_zoom.borrow_mut() = Some(Rc::new(f));
    }

    pub fn connect_page(self: &Rc<Self>, f: impl Fn(&Rc<PdfTab>) + 'static) {
        *self.on_page.borrow_mut() = Some(Rc::new(f));
    }

    pub fn connect_outline(self: &Rc<Self>, f: impl Fn(&Rc<PdfTab>) + 'static) {
        *self.on_outline.borrow_mut() = Some(Rc::new(f));
    }

    /// Called once the document is open and its pages are known, and again after a reload.
    pub fn connect_opened(self: &Rc<Self>, f: impl Fn(&Rc<PdfTab>) + 'static) {
        *self.on_open.borrow_mut() = Some(Rc::new(f));
    }

    /// Whether the render thread is still opening the document.
    pub fn opening(&self) -> bool {
        self.view.page_count() == 0 && !self.failed.get()
    }

    pub fn connect_matches(self: &Rc<Self>, f: impl Fn(&Rc<PdfTab>) + 'static) {
        *self.on_matches.borrow_mut() = Some(Rc::new(f));
    }

    pub fn connect_uri(&self, f: impl Fn(&str) + 'static) {
        *self.on_uri.borrow_mut() = Some(Rc::new(f));
    }

    fn emit(self: &Rc<Self>, hook: &Hook) {
        // Cloned out of the cell first: a handler is free to reach back into this tab.
        let handler = hook.borrow().clone();
        if let Some(f) = handler {
            f(self);
        }
    }

    fn show_status(&self, message: &str) {
        let (title, body) = match pdf::available() {
            true => ("Cannot Open This PDF", message.to_string()),
            false => (
                "PDF Support Is Not Available",
                format!(
                    "libpdfium was not found in {}.",
                    pdf::library_dir().display()
                ),
            ),
        };
        self.status.set_title(title);
        self.status.set_description(Some(&body));
        self.stack.set_visible_child_name("status");
    }

    fn ask(&self, request: Request) {
        let tx = self.tx.borrow();
        if let Some(tx) = tx.as_ref() {
            let _ = tx.send(request);
        }
    }

    fn push_history(&self) {
        let mut history = self.history.borrow_mut();
        history.push(self.view.anchor());
        if history.len() > HISTORY {
            history.remove(0);
        }
        self.future.borrow_mut().clear();
    }
}

impl PdfTab {
    /// Start the render thread, which opens the document and then answers requests for it.
    ///
    /// The thread owns the `PdfDoc` for its whole life, opening included. Nothing else may touch
    /// pdfium while it runs: the library is serialised by one process-wide lock, and two threads
    /// inside it abort the process.
    fn start(self: &Rc<Self>) -> Result<(), String> {
        if !pdf::available() {
            return Err("libpdfium was not found".to_string());
        }
        let path = self.path();
        let (tx, rx) = channel::<Request>();
        let weak = glib::SendWeakRef::from(self.view.downgrade());
        std::thread::Builder::new()
            .name("accent-pdf".to_string())
            .spawn(move || {
                let doc = match PdfDoc::open(&path) {
                    Ok(doc) => doc,
                    Err(e) => return send(&weak, Reply::Failed(format!("{e:#}"))),
                };
                let sizes = page_sizes(&doc);
                if sizes.is_empty() {
                    return send(&weak, Reply::Failed("This file has no pages.".to_string()));
                }
                send(&weak, Reply::Reloaded(sizes));
                render_loop(doc, path, rx, weak);
            })
            .map_err(|e| format!("cannot start the renderer: {e}"))?;
        *self.tx.borrow_mut() = Some(tx);
        Ok(())
    }

    /// Hook up one of the two views: what it wants rendered, and what comes back.
    fn wire(self: &Rc<Self>, view: &PdfView) {
        view.connect_wants(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_, scale, dark, wants| {
                tab.ask(Request::Tiles {
                    scale,
                    dark,
                    theme: theme_of(dark),
                    wants,
                });
            }
        ));
        view.connect_reply(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_, reply| tab.on_reply(reply)
        ));
        view.connect_page(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |page| {
                // Links are fetched per page, the first time one comes into view.
                if !tab.links.borrow().contains_key(&page) {
                    tab.ask(Request::Links(page));
                }
                tab.thumbs.queue_draw();
                tab.emit(&tab.on_page);
            }
        ));
        view.connect_goto(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |page| tab.goto_page(page)
        ));
        view.connect_pressed(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |view, x, y| tab.click(view, x, y)
        ));
        view.connect_select(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_, page, from, to| tab.selected_between(page, from, to)
        ));
        view.connect_motion(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |view, x, y| {
                // The pointer only changes when the answer does: a GDK call per pixel of travel
                // is what the editor's link hover deliberately avoids too.
                let over = tab.link_at(view, x, y).is_some();
                let on_page = view.page_point(x, y).is_some();
                view.set_cursor_from_name(Some(match (over, on_page) {
                    (true, _) => "pointer",
                    // A page is text to be dragged across, and says so before anyone tries.
                    (false, true) => "text",
                    (false, false) => "default",
                }));
            }
        ));
    }

    /// The Ctrl-hover link preview: where a link goes, without going there.
    ///
    /// A controller of its own rather than the cursor hook next door, for two reasons: that hook's
    /// signature drops the controller, and so the modifier state with it, and this one can refuse
    /// the event on the modifier alone. A pointer crossing a page without Ctrl held therefore
    /// costs one bit test and never looks a link up, let alone asks for a render.
    fn wire_preview(self: &Rc<Self>) {
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |controller, x, y| tab.hover(x, y, controller.current_event_state())
        ));
        // The pointer left the page for the sidebar, the chrome or another window.
        motion.connect_leave(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_| tab.hide_preview()
        ));
        self.view.add_controller(motion);
        // The tab was switched away from or closed while a preview was up.
        self.host.connect_unmap(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_| tab.hide_preview()
        ));
        // The band arrives after the popover does, for any page nobody has read yet.
        self.view.connect_lowres(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |ready| {
                let waiting = match tab.preview.borrow().as_ref().map(|p| p.target.clone()) {
                    Some(LinkTarget::Page { page, top }) if page as u32 == ready => {
                        Some((page, top))
                    }
                    _ => None,
                };
                if let Some((page, top)) = waiting {
                    tab.fill_band(page, top);
                }
            }
        ));
    }

    /// Ctrl over a link: show where it leads. Anything else takes the preview away.
    fn hover(self: &Rc<Self>, x: f64, y: f64, state: gtk::gdk::ModifierType) {
        if !state.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
            return self.hide_preview();
        }
        let Some(target) = self.link_at(&self.view, x, y) else {
            return self.hide_preview();
        };
        // Still the same link: the popover already says what this one does.
        if self
            .preview
            .borrow()
            .as_ref()
            .is_some_and(|shown| shown.target == target)
        {
            return;
        }
        self.hide_preview();
        self.show_preview(target, x, y);
    }

    fn hide_preview(&self) {
        if let Some(shown) = self.preview.borrow_mut().take() {
            shown.popover.popdown();
        }
    }

    /// Put a popover over the link. A page target gets a band of the page it leads to, an
    /// external one gets the address it would open, which is the thing worth knowing before
    /// clicking it.
    fn show_preview(self: &Rc<Self>, target: LinkTarget, x: f64, y: f64) {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let band = adw::Bin::new();
        match &target {
            LinkTarget::Page { page, .. } => {
                band.set_size_request(pdfview::LOWRES_W, BAND);
                content.append(&band);
                content.append(
                    &gtk::Label::builder()
                        .label(format!("Page {}", page + 1))
                        .css_classes(["caption", "dim-label"])
                        .xalign(0.0)
                        .build(),
                );
            }
            LinkTarget::Uri(uri) => content.append(
                &gtk::Label::builder()
                    .label(uri)
                    .css_classes(["caption"])
                    .wrap(true)
                    .max_width_chars(48)
                    .xalign(0.0)
                    .build(),
            ),
        }
        // Never autohide: an autohiding popover takes a grab, and this one is under the pointer
        // that is still reading the page. It cannot be targeted either, so it neither swallows a
        // click nor steals the crossing event that would take it away again.
        let popover = gtk::Popover::builder()
            .autohide(false)
            .can_target(false)
            .position(gtk::PositionType::Top)
            .child(&content)
            .build();
        popover.set_parent(&self.host);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        // A popover parented by hand stays parented until it is unparented by hand.
        popover.connect_closed(|popover| popover.unparent());
        popover.popup();
        let page = match target {
            LinkTarget::Page { page, top } => Some((page, top)),
            LinkTarget::Uri(_) => None,
        };
        *self.preview.borrow_mut() = Some(Preview {
            popover,
            band,
            target,
        });
        if let Some((page, top)) = page {
            self.fill_band(page, top);
        }
    }

    /// Slide the target page behind the band, rendering its low-resolution stand-in first if this
    /// is a page nobody has looked at.
    ///
    /// Only ever reached from a Ctrl-hover that landed on a link the popover is not already
    /// showing, so the pdfium lock is taken for one 256 px page render at most, and never once
    /// per pointer event.
    fn fill_band(self: &Rc<Self>, page: usize, top: Option<f32>) {
        let dark = self.view.dark();
        let ready = self.view.cache().borrow().lowres(page as u32, dark);
        let Some(texture) = ready else {
            return self.ask(Request::Tiles {
                scale: 1.0,
                dark,
                theme: theme_of(dark),
                wants: vec![Want {
                    page: page as u32,
                    tx: u16::MAX,
                    ty: u16::MAX,
                }],
            });
        };
        let Some((_, page_h)) = self.view.page_size(page) else {
            return;
        };
        let preview = self.preview.borrow();
        let Some(band) = preview.as_ref().map(|shown| shown.band.clone()) else {
            return;
        };
        drop(preview);
        if band.child().is_some() {
            return;
        }
        let offset = band_offset(top, page_h, texture.height() as f32);
        let strip = crop(&texture, offset, BAND);
        let picture = gtk::Picture::for_paintable(&strip);
        picture.set_size_request(strip.width(), strip.height());
        band.set_child(Some(&picture));
    }

    /// The keys a reader uses. Page Up, Page Down, Home and End are `GtkScrolledWindow`'s own.
    fn wire_keys(self: &Rc<Self>) {
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| {
                let shift = state.contains(gtk::gdk::ModifierType::SHIFT_MASK);
                if key == gtk::gdk::Key::c && state.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
                    tab.copy_selection();
                    return glib::Propagation::Stop;
                }
                match key {
                    gtk::gdk::Key::space if shift => tab.previous_page(),
                    gtk::gdk::Key::space => tab.next_page(),
                    gtk::gdk::Key::n => tab.next_page(),
                    gtk::gdk::Key::p => tab.previous_page(),
                    _ => return glib::Propagation::Proceed,
                }
                glib::Propagation::Stop
            }
        ));
        // Ctrl let go with the pointer still on the link: the preview was only ever the
        // modifier's, so it goes with it.
        keys.connect_key_released(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_, key, _, _| {
                if matches!(key, gtk::gdk::Key::Control_L | gtk::gdk::Key::Control_R) {
                    tab.hide_preview();
                }
            }
        ));
        self.view.add_controller(keys);
    }

    /// A drag across a page selected the text between two points on it.
    ///
    /// The glyphs are fetched the first time a page is dragged on and kept afterwards, so the
    /// first drag on a page may land a moment late and every one after it is immediate.
    fn selected_between(self: &Rc<Self>, page: usize, from: (f32, f32), to: (f32, f32)) {
        if self.glyphs.borrow().contains_key(&page) {
            self.pending_select.set(None);
            return self.select(page, from, to);
        }
        self.pending_select.set(Some((page, from, to)));
        self.ask(Request::Text(page));
    }

    /// Mark every glyph between the two points and remember the text they spell.
    fn select(&self, page: usize, from: (f32, f32), to: (f32, f32)) {
        let glyphs = self.glyphs.borrow();
        let Some(glyphs) = glyphs.get(&page) else {
            return;
        };
        let (Some(a), Some(b)) = (nearest(glyphs, from), nearest(glyphs, to)) else {
            return;
        };
        let (start, end) = (a.min(b), a.max(b));
        let picked = &glyphs[start..=end];
        *self.selected.borrow_mut() = picked.iter().map(|g| g.ch).collect();
        // A glyph with no box of its own — a space between words — would paint as a dot.
        let boxes: Vec<pdf::Rect> = picked
            .iter()
            .map(|g| g.rect)
            .filter(|r| r.width() > 0.0 && r.height() > 0.0)
            .collect();
        self.view.set_selection(Some((page, boxes)));
    }

    /// Drop the selection, on a click that is not a drag.
    fn clear_selection(&self) {
        if self.selected.borrow().is_empty() {
            return;
        }
        self.selected.borrow_mut().clear();
        self.view.set_selection(None);
    }

    /// A click on the page: follow a link if there is one under it.
    fn click(self: &Rc<Self>, view: &PdfView, x: f64, y: f64) {
        self.clear_selection();
        let Some(target) = self.link_at(view, x, y) else {
            return;
        };
        match target {
            LinkTarget::Page { page, top } => {
                self.push_history();
                self.view.goto_page(page, top);
            }
            LinkTarget::Uri(uri) => {
                let handler = self.on_uri.borrow().clone();
                if let Some(f) = handler {
                    f(&uri);
                }
            }
        }
    }

    fn link_at(&self, view: &PdfView, x: f64, y: f64) -> Option<LinkTarget> {
        let (page, px, py) = view.page_point(x, y)?;
        let links = self.links.borrow();
        links.get(&page)?.iter().find_map(|link| {
            let r = link.rect;
            let inside = px >= r.left && px <= r.right && py >= r.top && py <= r.bottom;
            inside.then(|| link.target.clone())
        })
    }

    /// Everything from the render thread that is not a texture.
    fn on_reply(self: &Rc<Self>, reply: Reply) {
        match reply {
            Reply::Links(page, links) => {
                self.links.borrow_mut().insert(page, links);
            }
            Reply::Text(page, glyphs) => {
                self.glyphs.borrow_mut().insert(page, glyphs);
                // The drag that asked for them is usually still going, so answer it now rather
                // than making the user drag again.
                let pending = self.pending_select.get();
                if let Some((at, from, to)) = pending.filter(|(at, _, _)| *at == page) {
                    self.select(page, from, to);
                    let _ = at;
                }
            }
            Reply::Outline(outline) => {
                *self.outline.borrow_mut() = outline;
                self.emit(&self.on_outline);
            }
            Reply::Found { query, page, hits } => {
                // A result for a query the user has already moved past.
                if query != self.query.get() {
                    return;
                }
                let mut matches = self.matches.borrow_mut();
                for hit in &hits {
                    if let Some(rect) = hit.iter().copied().reduce(pdf::Rect::union) {
                        matches.push((page, rect));
                    }
                }
                // Kept in page order, which is the order a reader steps through them.
                matches.sort_by_key(|(page, _)| *page);
                let mut marks: std::collections::HashMap<usize, Vec<pdf::Rect>> =
                    std::collections::HashMap::new();
                for (page, rect) in matches.iter() {
                    marks.entry(*page).or_default().push(*rect);
                }
                drop(matches);
                self.view.set_marks(marks);
                self.emit(&self.on_matches);
            }
            Reply::Reloaded(sizes) => {
                // The anchor is taken now rather than when the reload was asked for: the reader
                // may have moved while the file was being re-read.
                let anchor = self.view.anchor().clamped(sizes.len());
                self.view.forget_textures();
                self.thumbs.forget_textures();
                self.links.borrow_mut().clear();
                self.view.set_sizes(sizes.clone());
                self.thumbs.set_sizes(sizes);
                match self.pending.take() {
                    // The document just opened: go where the session left the reader.
                    Some(place) => self.view.goto_page(place.page, None),
                    None => self.view.scroll_to(anchor),
                }
                self.ask(Request::Outline);
                self.emit(&self.on_open);
            }
            Reply::Failed(message) => {
                self.failed.set(true);
                self.show_status(&message);
                self.emit(&self.on_open);
            }
            // Textures never reach here; `PdfView::deliver` keeps those.
            Reply::Tile(..) | Reply::Lowres { .. } => {}
        }
    }
}

/// How far to slide a low-resolution page up so the band shows what a link points at.
///
/// `top` is points down the target page, `page_h` its height in points and `texture_h` the
/// stand-in's height in pixels. The destination is centred in the band, and the band stays inside
/// the page at both ends: a link to the last line shows the foot of the page rather than a strip
/// of nothing under it.
fn band_offset(top: Option<f32>, page_h: f32, texture_h: f32) -> i32 {
    let at = top.unwrap_or(0.0) / page_h.max(1.0) * texture_h;
    let band = BAND as f32;
    (at - band / 2.0).clamp(0.0, (texture_h - band).max(0.0)) as i32
}

/// One horizontal band of a texture, as a texture of its own.
///
/// The crop is of the pixels and not of the layout: a clipped widget still asks for the whole
/// page's height, and a popover is as big as what it holds asks to be.
fn crop(texture: &gtk::gdk::MemoryTexture, top: i32, height: i32) -> gtk::gdk::MemoryTexture {
    let (w, h) = (texture.width(), texture.height());
    let height = height.min(h);
    let top = top.clamp(0, h - height);
    let stride = w as usize * 4;
    let mut pixels = vec![0u8; stride * h as usize];
    texture.download(&mut pixels, stride);
    let from = top as usize * stride;
    let bytes = glib::Bytes::from(&pixels[from..from + stride * height as usize]);
    // The layout `GdkTexture::download` writes, on every platform GTK builds for.
    gtk::gdk::MemoryTexture::new(
        w,
        height,
        gtk::gdk::MemoryFormat::B8g8r8a8Premultiplied,
        &bytes,
        stride,
    )
}

/// The glyph nearest a point on the page, which is the one a drag means to start or end on.
///
/// A hit inside a glyph's own box wins outright; otherwise the closest box by the distance from
/// the point to it, so a drag through the margin still catches the line it is level with.
fn nearest(glyphs: &[pdf::Glyph], (x, y): (f32, f32)) -> Option<usize> {
    let mut best: Option<(f32, usize)> = None;
    for (i, glyph) in glyphs.iter().enumerate() {
        let r = glyph.rect;
        if x >= r.left && x <= r.right && y >= r.top && y <= r.bottom {
            return Some(i);
        }
        // Distance to the box, zero along an axis the point already lies within.
        let dx = (r.left - x).max(0.0).max(x - r.right);
        let dy = (r.top - y).max(0.0).max(y - r.bottom);
        let d = dx * dx + dy * dy;
        if best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, i));
        }
    }
    best.map(|(_, i)| i)
}

/// Every page's size in points, which is all the widget needs to lay the document out.
///
/// One pdfium call for the whole document, not one per page: asking a loaded page for its size
/// costs a full parse of that page, and 1 554 of those is eleven seconds before anything appears.
fn page_sizes(doc: &PdfDoc) -> Vec<(f32, f32)> {
    doc.page_sizes().unwrap_or_default()
}

/// The render thread.
///
/// One request at a time, with one twist: before every tile it drains the queue, so a batch that
/// has been overtaken by a scroll is abandoned rather than rendered into a viewport nobody is
/// looking at any more.
fn render_loop(
    mut doc: PdfDoc,
    path: PathBuf,
    rx: std::sync::mpsc::Receiver<Request>,
    view: glib::SendWeakRef<PdfView>,
) {
    // The channel closing is the tab going away, which is the only way this thread ends.
    while let Ok(first) = rx.recv() {
        let mut request = Some(first);
        while let Some(current) = request.take() {
            match current {
                Request::Tiles {
                    scale,
                    dark,
                    theme,
                    wants,
                } => {
                    for want in wants {
                        // Anything newer wins: the viewport it was for has moved.
                        match rx.try_recv() {
                            Ok(newer) => {
                                request = Some(newer);
                                break;
                            }
                            Err(TryRecvError::Disconnected) => return,
                            Err(TryRecvError::Empty) => {}
                        }
                        render_want(&doc, &view, scale, dark, theme, want);
                    }
                }
                Request::Links(page) => {
                    if let Ok(links) = doc.links(page) {
                        send(&view, Reply::Links(page, links));
                    }
                }
                Request::Text(page) => {
                    if let Ok(glyphs) = doc.page_text(page) {
                        send(&view, Reply::Text(page, glyphs));
                    }
                }
                Request::Outline => {
                    if let Ok(outline) = doc.outline() {
                        send(&view, Reply::Outline(outline));
                    }
                }
                Request::Search { query, text } => {
                    for page in 0..doc.page_count() {
                        // Between pages rather than between matches: loading a page's text is
                        // the expensive part and is not worth abandoning halfway.
                        match rx.try_recv() {
                            Ok(newer) => {
                                request = Some(newer);
                                break;
                            }
                            Err(TryRecvError::Disconnected) => return,
                            Err(TryRecvError::Empty) => {}
                        }
                        match doc.search(page, &text) {
                            Ok(hits) if !hits.is_empty() => {
                                send(&view, Reply::Found { query, page, hits })
                            }
                            _ => {}
                        }
                    }
                }
                Request::Reload => {
                    // Swapped only on success: a half-written PDF fails to open often while a
                    // LaTeX run is going, and the next event tries again.
                    match PdfDoc::open(&path) {
                        Ok(fresh) => {
                            doc = fresh;
                            send(&view, Reply::Reloaded(page_sizes(&doc)));
                        }
                        Err(e) => tracing::debug!("reloading {}: {e:#}", path.display()),
                    }
                }
            }
        }
    }
}

/// Render one wanted tile, or the low-resolution stand-in for a whole page.
fn render_want(
    doc: &PdfDoc,
    view: &glib::SendWeakRef<PdfView>,
    scale: f32,
    dark: bool,
    theme: pdf::Theme,
    want: Want,
) {
    let page = want.page as usize;
    if want.is_lowres() {
        let Ok((w, _)) = doc.page_size(page) else {
            return;
        };
        let low = pdfview::LOWRES_W as f32 / w.max(1.0);
        if let Ok(image) = doc.render_page(page, low, theme) {
            send(
                &view.clone(),
                Reply::Lowres {
                    page: want.page,
                    dark,
                    image,
                },
            );
        }
        return;
    }
    let Ok((w, h)) = doc.page_size(page) else {
        return;
    };
    let (full_w, full_h) = ((w * scale).round() as i32, (h * scale).round() as i32);
    let (x, y) = (
        i32::from(want.tx) * pdfview::TILE,
        i32::from(want.ty) * pdfview::TILE,
    );
    // Clamped to the page: pdfium clears only the page's own area, so a tile hanging off the
    // edge would come back with uninitialised pixels in it.
    let (tw, th) = (pdfview::TILE.min(full_w - x), pdfview::TILE.min(full_h - y));
    if tw <= 0 || th <= 0 {
        return;
    }
    if let Ok(image) = doc.render_tile(page, scale, x, y, tw, th, theme) {
        let key = TileKey {
            page: want.page,
            scale_milli: (scale * 1000.0).round() as u32,
            tx: want.tx,
            ty: want.ty,
            dark,
        };
        send(view, Reply::Tile(key, image));
    }
}

/// How the pages of a document are coloured under the theme in force.
///
/// Must be called on the main thread: `theme.rs` holds the chosen theme in thread-local state.
fn theme_of(dark: bool) -> pdf::Theme {
    match crate::theme::pdf_colours(dark) {
        Some((paper, ink)) => pdf::Theme::Recolour { paper, ink },
        None => pdf::Theme::Plain,
    }
}

/// Hand one answer to the main loop.
///
/// Each reply travels in its own idle callback holding a weak reference to the view, so a tab
/// closed while a render was in flight simply drops the result.
fn send(view: &glib::SendWeakRef<PdfView>, reply: Reply) {
    let view = view.clone();
    glib::idle_add_once(move || {
        if let Some(view) = view.upgrade() {
            view.deliver(reply);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{BAND, band_offset};

    /// The band follows the destination but never runs off either end of the page.
    #[test]
    fn a_band_is_centred_on_the_destination_and_stays_on_the_page() {
        let (page_h, texture_h) = (800.0, 400.0);
        // Halfway down the page, so the band is centred on the middle of the stand-in.
        assert_eq!(band_offset(Some(400.0), page_h, texture_h), 200 - BAND / 2);
        // The top of the page, and a destination with no y at all, both start at the top.
        assert_eq!(band_offset(Some(0.0), page_h, texture_h), 0);
        assert_eq!(band_offset(None, page_h, texture_h), 0);
        // The last line shows the foot of the page rather than a strip of nothing under it.
        assert_eq!(band_offset(Some(800.0), page_h, texture_h), 400 - BAND);
        // A page shorter than the band does not scroll at all.
        assert_eq!(band_offset(Some(400.0), page_h, 50.0), 0);
    }
}
