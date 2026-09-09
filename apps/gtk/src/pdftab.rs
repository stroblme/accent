//! A PDF in a tab: the document, the thread that renders it, and the reading state around it.
//!
//! The widget in `pdfview.rs` knows nothing about pdfium; it asks for tiles and paints what it is
//! given. This is the other half: one thread per open document, which owns the `PdfDoc` and
//! answers those requests, plus the history, the search and the outline that make it a reader
//! rather than a viewer.
//!
//! pdfium is serialised behind one process-wide lock (see `accent_core::pdf`), so one thread per
//! document is not a limitation we could lift by adding more.

use crate::pdfview::{self, Anchor, PdfView, PdfZoom, Reply, Span, TileKey, Want};
use crate::ring;
use accent_api::PdfLink;
use accent_core::pdf::{self, LinkTarget, PdfDoc};
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{Sender, TryRecvError, channel};

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
        /// The first page still to look at. A query the reader interrupted comes back with this
        /// moved on, so it finishes the document instead of stopping where it was pushed aside.
        from: usize,
    },
    /// Where the note links that highlight this document land on the page today.
    Highlights(Vec<PdfLink>),
    /// Write those links into the file as real `/Highlight` annotations, in `color`.
    Export {
        links: Vec<PdfLink>,
        color: [u8; 3],
    },
    /// One free-hand stroke, in that page's own points, drawn the way its tool draws.
    Ink {
        page: usize,
        points: Vec<(f32, f32)>,
        style: pdf::InkStyle,
    },
    /// Take off whichever stroke passes within [`ERASE_RADIUS`] of this point.
    Erase {
        page: usize,
        at: (f32, f32),
    },
    /// Undo the last stroke drawn in this session.
    Undo,
    /// Write the drawn-on document out, if anything was drawn since the last time. The channel,
    /// where there is one, is told when that is done — which is what the window close waits on.
    Save(Option<Sender<()>>),
    Reload,
}

/// How close the eraser has to pass to a stroke to take it. Whole strokes, never part of one.
const ERASE_RADIUS: f32 = 4.0;

/// How long after the last stroke the document is written out.
const INK_SAVE: std::time::Duration = std::time::Duration::from_secs(1);

/// What the render thread knows about the ink it has drawn, so that Undo can reach this session's
/// strokes and nothing else.
///
/// `base` is how many annotations a page carried before we touched it — they come first in
/// `/Annots`, so an erase below that line moves it. `added` is the pages drawn on, in order.
#[derive(Default)]
struct Ink {
    base: HashMap<usize, usize>,
    added: Vec<usize>,
    dirty: bool,
}

impl Ink {
    /// A page is about to be drawn on: remember what was on it before, once.
    fn note(&mut self, page: usize, count: usize) {
        self.base.entry(page).or_insert(count);
    }

    /// An annotation at `index` was removed from `page`.
    ///
    /// Below the line it was one the document already had, so the line moves down with it; above
    /// it, it was one of ours and there is one fewer stroke left to undo.
    fn erased(&mut self, page: usize, index: usize) {
        match self.base.get_mut(&page) {
            Some(base) if index < *base => *base -= 1,
            _ => {
                if let Some(at) = self.added.iter().rposition(|at| *at == page) {
                    self.added.remove(at);
                }
            }
        }
    }

    /// Which annotation of `page` Undo should delete, given how many it now has.
    fn undo_target(&self, page: usize, count: usize) -> Option<usize> {
        let base = self.base.get(&page).copied().unwrap_or(count);
        (count > base).then(|| count - 1)
    }
}

/// Where a document is being read, remembered per file in the session.
pub use accent_core::config::PdfPlace as Place;

type Hook = RefCell<Option<Rc<dyn Fn(&Rc<PdfTab>)>>>;
/// A highlight was clicked: the note holding the link, and the byte it starts at.
type NoteHook = RefCell<Option<Rc<dyn Fn(&str, usize)>>>;
/// An export finished, with what it wrote or why it could not.
type ExportHook = RefCell<Option<Rc<dyn Fn(&Rc<PdfTab>, Result<usize, String>)>>>;
type UriHook = RefCell<Option<Rc<dyn Fn(&str)>>>;

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
    /// The drawing tools, floating over the page while the window says they are wanted.
    ring: Rc<ring::Ring>,
    /// The strip the thumbnails live in, built once. Handing the Outline pane a fresh
    /// `GtkScrolledWindow` around the same widget every time would re-parent a widget that
    /// already has a parent, which GTK refuses with a critical.
    thumb_strip: gtk::ScrolledWindow,
    /// Requests to the render thread. Dropping it is what ends the thread, so it is dropped with
    /// the tab and nothing else has to be joined.
    tx: RefCell<Option<Sender<Request>>>,
    /// Colours inverted against the system's choice, for a document that renders badly either way.
    inverted: Cell<bool>,
    /// The palette the cached tiles were rendered in, so a theme change can tell that they are
    /// of the old one. See [`PdfTab::restyle`].
    theme: Cell<pdf::Theme>,
    /// The document could not be opened, so it is not opening either.
    failed: Cell<bool>,
    /// The zoom to restore when presentation mode ends.
    presenting: Cell<Option<PdfZoom>>,
    links: RefCell<std::collections::HashMap<usize, Vec<pdf::Link>>>,
    /// Each page's glyphs, fetched the first time someone drags across that page.
    glyphs: RefCell<std::collections::HashMap<usize, Vec<pdf::Glyph>>>,
    /// The selected text, for Ctrl+C.
    selected: RefCell<String>,
    /// The same selection as glyph ranges, one per page it covers, which is what a link is made
    /// of. Kept beside the text because the boxes on screen cannot be turned back into indices.
    ranges: RefCell<Vec<pdf::Selection>>,
    /// The note links that highlight this document, as the index last reported them. The painted
    /// quads carry an index into this, so a click on one knows which note to open.
    notes: RefCell<Vec<PdfLink>>,
    /// A page and selection to show once the glyphs for it arrive: Follow Link into a PDF.
    pending_show: Cell<Option<(usize, Option<[usize; 4]>)>>,
    /// A save is already scheduled, so a burst of strokes costs one write.
    save_pending: Cell<bool>,
    /// The etag of the last write *this tab* made, so a watcher report of our own save is
    /// recognised and not answered with a reload. See [`PdfTab::refresh`].
    saved: Cell<Option<accent_core::fs::Etag>>,
    outline: RefCell<Vec<pdf::Outline>>,
    /// The link preview on screen, if the pointer is on a link with Ctrl held.
    preview: RefCell<Option<Preview>>,
    /// A drag that arrived before the glyphs of every page it covers did, to answer when the
    /// last of them lands.
    pending_select: Cell<Option<Span>>,
    /// Where the session says this document was left, until the first page sizes arrive and it
    /// can be applied. `None` afterwards, so a reload keeps the reader where they are instead.
    pending: Cell<Option<Place>>,
    /// Which search these results belong to, so a stale page's answer is dropped.
    query: Cell<u64>,
    matches: RefCell<Vec<(usize, pdf::Rect)>>,
    current: Cell<Option<usize>>,
    on_zoom: Hook,
    on_page: Hook,
    /// Fired just before a jump, so the pane can record where the reader was.
    on_jump: Hook,
    on_outline: Hook,
    /// Fired when the document's pages are known, which is when it stops being "opening".
    on_open: Hook,
    on_matches: Hook,
    on_uri: UriHook,
    on_mode: Hook,
    on_note: NoteHook,
    on_export: ExportHook,
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
    // The tools float over the page rather than sitting in the chrome, so the scroller goes in
    // an overlay and the ring is its one child.
    let ring = ring::Ring::new();
    let overlay = gtk::Overlay::builder().child(&scroller).build();
    overlay.add_overlay(ring.widget());
    let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
    host.append(&overlay);
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
        ring: ring.clone(),
        thumb_strip: gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&thumbs)
            .build(),
        tx: RefCell::new(None),
        inverted: Cell::new(false),
        theme: Cell::new(theme_of(adw::StyleManager::default().is_dark())),
        failed: Cell::new(false),
        presenting: Cell::new(None),
        pending: Cell::new(Some(place)),
        pending_select: Cell::new(None),
        links: RefCell::new(std::collections::HashMap::new()),
        glyphs: RefCell::new(std::collections::HashMap::new()),
        selected: RefCell::new(String::new()),
        ranges: RefCell::new(Vec::new()),
        notes: RefCell::new(Vec::new()),
        pending_show: Cell::new(None),
        save_pending: Cell::new(false),
        saved: Cell::new(None),
        outline: RefCell::new(Vec::new()),
        preview: RefCell::new(None),
        query: Cell::new(0),
        matches: RefCell::new(Vec::new()),
        current: Cell::new(None),
        on_zoom: RefCell::new(None),
        on_page: RefCell::new(None),
        on_jump: RefCell::new(None),
        on_outline: RefCell::new(None),
        on_open: RefCell::new(None),
        on_matches: RefCell::new(None),
        on_uri: RefCell::new(None),
        on_mode: RefCell::new(None),
        on_note: RefCell::new(None),
        on_export: RefCell::new(None),
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
    tab.wire_menu();

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
    ///
    /// A page is rendered in the theme's colours rather than recoloured afterwards, so every
    /// texture in the cache belongs to one palette. Switching between two themes of the same
    /// darkness — Adwaita to Solarized — leaves `dark` alone and every cached tile wrong, which
    /// is why the palette itself is what is compared here.
    pub fn restyle(&self) {
        let dark = adw::StyleManager::default().is_dark() != self.inverted.get();
        let theme = theme_of(dark);
        if self.theme.replace(theme) != theme {
            self.view.forget_textures();
            self.thumbs.forget_textures();
        }
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

    /// The status bar's other readout: where in the document the reader is, which is what a PDF
    /// has to say in the slot a note fills with its word count.
    pub fn page_label(&self) -> Option<String> {
        crate::statusbar::page_label(self.view.current_page(), self.view.page_count())
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
        self.jumping();
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

    /// Where the reader is, and how to put them back there. The pane's history is what holds
    /// these; a PDF keeps no stack of its own.
    pub fn anchor(&self) -> Anchor {
        self.view.anchor()
    }

    pub fn scroll_to(&self, anchor: Anchor) {
        self.view.scroll_to(anchor);
    }

    /// The reading view's geometry, for `ACCENT_BENCH_PDF` and nothing else: the headless image
    /// has no pointer and no window manager, so the numbers a fit produced are the only way to
    /// see that it fitted.
    pub fn geometry(&self) -> String {
        self.view.geometry()
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

    /// Copy the selection as a wikilink into this PDF, with the selected text as its alias.
    ///
    /// Pasting that link into a note is what makes it a highlight: the index sees a link into a
    /// page and a selection, and the viewer paints it. There is no separate Highlight action,
    /// because a highlight is a link and the clipboard is how a link gets where it is wanted.
    ///
    /// One link per page, joined by newlines: `page=N&selection=…` names one page, and a drag
    /// that ran across a break is two places in the document.
    pub fn copy_link(&self) {
        // A loose PDF is linked by name: an absolute path in a wikilink resolves nowhere, and
        // the name is what a vault would key it by if the file ever joined one.
        let key = self.key.borrow().clone();
        let rel = match crate::doc::is_loose_key(&key) {
            true => crate::doc::file_name(&key).to_string(),
            false => key,
        };
        let glyphs = self.glyphs.borrow();
        let links: Vec<String> = self
            .ranges
            .borrow()
            .iter()
            .filter_map(|sel| {
                let out = pdf::selection_link(glyphs.get(&sel.page)?, &rel, sel);
                Some(link_with_alias(&out.link, &out.text))
            })
            .collect();
        if links.is_empty() {
            return;
        }
        self.view.clipboard().set_text(&links.join("\n"));
    }

    /// The note links that highlight this document, as the index reports them. Painting them
    /// needs the pages' glyphs, so the render thread answers.
    pub fn set_note_links(self: &Rc<Self>, links: Vec<PdfLink>) {
        *self.notes.borrow_mut() = links.clone();
        self.ask(Request::Highlights(links));
    }

    /// Write those highlights into the file itself, as `/Highlight` annotations in `color`.
    pub fn export_highlights(self: &Rc<Self>, color: [u8; 3]) {
        let links = self.notes.borrow().clone();
        self.ask(Request::Export { links, color });
    }

    /// Go to a page, and show the selection a link names as if it had just been dragged.
    ///
    /// The glyphs of that page are what turn the four numbers back into rectangles, so a page
    /// never read before answers a moment later rather than not at all.
    pub fn show_link(self: &Rc<Self>, page: usize, selection: Option<[usize; 4]>) {
        self.pending_show.set(Some((page, selection)));
        // Nothing is known about the document yet; the reload reply applies it.
        if self.view.page_count() == 0 {
            return;
        }
        self.jumping();
        match self.glyphs.borrow().contains_key(&page) {
            true => self.apply_show(),
            false => self.ask(Request::Text(page)),
        }
    }

    /// Show whatever [`PdfTab::show_link`] is still waiting to show, if its page has arrived.
    fn apply_show(&self) {
        let Some((page, selection)) = self.pending_show.get() else {
            return;
        };
        let Some(sel) = selection else {
            self.pending_show.set(None);
            return self.view.goto_page(page, None);
        };
        let glyphs = self.glyphs.borrow();
        let Some(page_glyphs) = glyphs.get(&page) else {
            return;
        };
        self.pending_show.set(None);
        // A link written against another engine's numbering points at nothing here; the page it
        // names is still where the reader wanted to be.
        let Some((range, quads)) = pdf::selection_quads(page_glyphs, sel) else {
            return self.view.goto_page(page, None);
        };
        let bounds = quads.iter().copied().reduce(pdf::Rect::union);
        *self.selected.borrow_mut() = page_glyphs[range.clone()].iter().map(|g| g.ch).collect();
        *self.ranges.borrow_mut() = vec![pdf::Selection {
            page,
            start: range.start,
            end: range.end,
        }];
        drop(glyphs);
        self.view.set_selection(vec![(page, quads)]);
        match bounds {
            Some(r) => self.view.reveal(page, r),
            None => self.view.goto_page(page, None),
        }
    }

    /// What a drag over the page does: select, draw, or erase.
    pub fn mode(&self) -> pdfview::Mode {
        self.view.mode()
    }

    /// Only the reading view: the thumbnail strip is a list of buttons, never a canvas.
    pub fn set_mode(self: &Rc<Self>, mode: pdfview::Mode) {
        // The ring is told whatever the window decided, even when the mode did not change: it may
        // be a freshly built one that has never been told anything.
        self.ring.set_tool(mode);
        if self.view.mode() == mode {
            return;
        }
        self.view.set_mode(mode);
        // Putting the pen down is a good moment to write, rather than waiting out the timer.
        if mode == pdfview::Mode::Select {
            self.flush();
        }
        self.emit(&self.on_mode);
    }

    /// Show or hide the ring of tools.
    pub fn set_drawing(&self, showing: bool, at: Option<(f64, f64)>) {
        self.ring.set_visible(showing, at);
    }

    /// Where the reader has dragged the ring, so the next tab to show one puts it there.
    pub fn ring_at(&self) -> Option<(f64, f64)> {
        self.ring.at()
    }

    /// What the status bar says while a pen is out, or nothing while one is not.
    pub fn mode_label(&self) -> Option<&'static str> {
        match self.view.mode() {
            pdfview::Mode::Select => None,
            pdfview::Mode::Pen => Some("Pen"),
            pdfview::Mode::Highlighter => Some("Highlighter"),
            pdfview::Mode::Eraser => Some("Eraser"),
        }
    }

    /// Called when the pen is picked up or put down.
    pub fn connect_mode(&self, f: impl Fn(&Rc<PdfTab>) + 'static) {
        *self.on_mode.borrow_mut() = Some(Rc::new(f));
    }

    /// Write out whatever has been drawn, if anything has.
    ///
    /// The thread answers when it gets there; nothing waits for it, because the write is atomic
    /// and the channel is drained before the thread ends, so a tab closing still saves.
    pub fn flush(self: &Rc<Self>) {
        self.ask(Request::Save(None));
    }

    /// The same, but wait for it — the window is closing and the process is about to end, so a
    /// write still on the render thread's queue would go with it.
    ///
    // ponytail: up to a second of the main loop, and only on the way out. The thread answers as
    // soon as it finishes whatever tile it is on, so in practice this is a few milliseconds.
    pub fn flush_blocking(self: &Rc<Self>) {
        let (tx, rx) = channel();
        self.ask(Request::Save(Some(tx)));
        let _ = rx.recv_timeout(std::time::Duration::from_secs(1));
    }

    /// Write out a second after the last stroke, and once for a burst of them.
    ///
    /// The idiom the session save uses: a flag set once and cleared by its own callback, rather
    /// than a `SourceId` removed and replaced, which is a critical if the source has already run.
    fn save_soon(self: &Rc<Self>) {
        if self.save_pending.replace(true) {
            return;
        }
        glib::timeout_add_local_once(
            INK_SAVE,
            glib::clone!(
                #[weak(rename_to = tab)]
                self,
                move || {
                    tab.save_pending.set(false);
                    tab.flush();
                }
            ),
        );
    }

    /// Called when a highlight is clicked, with the note holding the link and the byte it is at.
    pub fn connect_note(&self, f: impl Fn(&str, usize) + 'static) {
        *self.on_note.borrow_mut() = Some(Rc::new(f));
    }

    /// Called when an export finishes, with what it wrote or why it could not.
    pub fn connect_export(&self, f: impl Fn(&Rc<PdfTab>, Result<usize, String>) + 'static) {
        *self.on_export.borrow_mut() = Some(Rc::new(f));
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
                from: 0,
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
    ///
    /// A write of our own is not a reason: the document in memory *is* what was written, and
    /// re-reading it would drop annotations made since. There is no `own: true` to ride on the
    /// way a note's save has one, because the bytes never went through the vault — so the etag
    /// of what we wrote is what tells the two apart.
    pub fn refresh(self: &Rc<Self>) {
        if self.saved.get().is_some()
            && accent_core::fs::Etag::of(&self.path()).ok() == self.saved.get()
        {
            return;
        }
        self.ask(Request::Reload);
    }

    pub fn connect_zoom(self: &Rc<Self>, f: impl Fn(&Rc<PdfTab>) + 'static) {
        *self.on_zoom.borrow_mut() = Some(Rc::new(f));
    }

    pub fn connect_jump(self: &Rc<Self>, f: impl Fn(&Rc<PdfTab>) + 'static) {
        *self.on_jump.borrow_mut() = Some(Rc::new(f));
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

    /// About to jump: the window records where the reader is, so Back returns here. Fired before
    /// the view moves, which is what makes `anchor()` still the place being left.
    fn jumping(self: &Rc<Self>) {
        self.emit(&self.on_jump);
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
        view.connect_clicked(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |view, x, y| tab.clicked_highlight(view, x, y)
        ));
        view.connect_ink(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |page, points| {
                let style = tab.view.mode().ink(crate::theme::accent_rgb());
                tab.ask(Request::Ink {
                    page,
                    points,
                    style,
                });
            }
        ));
        view.connect_erase(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |page, at| tab.ask(Request::Erase { page, at })
        ));
        view.connect_select(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_, span| tab.selected_between(span)
        ));
        view.connect_motion(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |view, x, y| {
                // The pointer only changes when the answer does: a GDK call per pixel of travel
                // is what the editor's link hover deliberately avoids too.
                // While a tool is out, the cursor says so and nothing here takes it back: the
                // page is not text to be selected, and a link is not to be followed.
                if view.mode() != pdfview::Mode::Select {
                    return;
                }
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
        let ready = self.view.cache().borrow_mut().lowres(page as u32, dark);
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

    /// The page's own menu, on a secondary click over it.
    ///
    /// Copy and Copy Link to Selection when there is a selection, then Export Highlights, which
    /// is about the document rather than about what is selected and so is always offered. The
    /// drawing tools are not here: they are the ring, which the header's Drawing button opens.
    ///
    /// `win.` actions rather than a group of the tab's own: that is what gives them a row in the
    /// palette and a rebindable accelerator, which is the whole argument of DESIGN.md's keyboard
    /// section. The tab keeps `Ctrl+C` in its key controller either way.
    fn wire_menu(self: &Rc<Self>) {
        let secondary = gtk::GestureClick::builder()
            .button(gtk::gdk::BUTTON_SECONDARY)
            .build();
        secondary.connect_pressed(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_, _, x, y| tab.selection_menu(x, y)
        ));
        self.view.add_controller(secondary);
    }

    /// Put the menu under the pointer.
    fn selection_menu(&self, x: f64, y: f64) {
        let menu = gio::Menu::new();
        // Window actions, in sections, the way the terminal's menu is built: that is what puts
        // them in the palette and lets them be rebound, which a tab-local group could not.
        if !self.selected.borrow().is_empty() {
            let clipboard = gio::Menu::new();
            for action in ["win.pdf-copy", "win.pdf-copy-link"] {
                clipboard.append(Some(crate::label_of(action)), Some(action));
            }
            menu.append_section(None, &clipboard);
        }
        let file = gio::Menu::new();
        file.append(
            Some(crate::label_of("win.pdf-export-highlights")),
            Some("win.pdf-export-highlights"),
        );
        menu.append_section(None, &file);
        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        // Parented to the box rather than to the view, and pointed at the box's own coordinates:
        // a popover hung off a widget with a `size_allocate` of its own never re-presents and
        // freezes at its first-frame size (DESIGN.md, States).
        let at = gtk::graphene::Point::new(x as f32, y as f32);
        let at = self.view.compute_point(&self.host, &at).unwrap_or(at);
        popover.set_parent(&self.host);
        popover.set_has_arrow(false);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
            at.x() as i32,
            at.y() as i32,
            1,
            1,
        )));
        // A popover parented by hand stays parented until it is unparented by hand — but not
        // while it is closing. `closed` is emitted from inside the item's own `clicked`, and an
        // unparented widget has no path to the action group on the host, so unparenting there
        // would drop the Copy the click had just asked for, exactly as it dropped the status
        // bar's Fit Page. The idle runs once the click is over.
        popover.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        popover.popup();
    }

    /// The keys a reader uses. Page Up, Page Down, Home and End are `GtkScrolledWindow`'s own;
    /// the arrows are not — it binds a scroll step to `Ctrl+Up`/`Ctrl+Down` and leaves the bare
    /// arrow keys to move the focus off the page — so they are wired here.
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
                // The pen's own two keys, on the tab like Copy: `Ctrl+Z` and `Escape` belong to
                // whatever has the keyboard, and here that is the page being drawn on.
                if tab.mode() != pdfview::Mode::Select {
                    if key == gtk::gdk::Key::z
                        && state.contains(gtk::gdk::ModifierType::CONTROL_MASK)
                    {
                        tab.ask(Request::Undo);
                        return glib::Propagation::Stop;
                    }
                    if key == gtk::gdk::Key::Escape {
                        tab.set_mode(pdfview::Mode::Select);
                        return glib::Propagation::Stop;
                    }
                }
                // Alt+Left and Alt+Right are Back and Forward, and Ctrl with an arrow is the
                // scroller's own step: only the bare key reads the document.
                let bare = !state.intersects(
                    gtk::gdk::ModifierType::CONTROL_MASK
                        | gtk::gdk::ModifierType::ALT_MASK
                        | gtk::gdk::ModifierType::SUPER_MASK,
                );
                match key {
                    gtk::gdk::Key::space if shift => tab.previous_page(),
                    gtk::gdk::Key::space => tab.next_page(),
                    gtk::gdk::Key::n => tab.next_page(),
                    gtk::gdk::Key::p => tab.previous_page(),
                    // A page back and a page forth whatever the zoom: horizontal movement is
                    // Shift and the wheel, and one key cannot mean two things.
                    gtk::gdk::Key::Left if bare => tab.previous_page(),
                    gtk::gdk::Key::Right if bare => tab.next_page(),
                    gtk::gdk::Key::Up if bare => tab.view.scroll_step(false),
                    gtk::gdk::Key::Down if bare => tab.view.scroll_step(true),
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

    /// A drag selected the text between two points, which may be on different pages.
    ///
    /// The glyphs are fetched the first time a page is dragged over and kept afterwards, so the
    /// first drag onto a page may land a moment late and every one after it is immediate. A drag
    /// that has crossed a page break wants every page it covers, and is answered as soon as it
    /// has them all.
    fn selected_between(self: &Rc<Self>, span: Span) {
        let missing: Vec<usize> = {
            let glyphs = self.glyphs.borrow();
            pages_of(span)
                .filter(|page| !glyphs.contains_key(page))
                .collect()
        };
        if missing.is_empty() {
            self.pending_select.set(None);
            return self.select(span);
        }
        // A drag reports on every motion event, so only pages the drag did not already cover are
        // asked for: nothing drops a `Text` request, so the first ask is always answered.
        let asked = self.pending_select.replace(Some(span));
        for page in missing {
            if asked.is_none_or(|before| !pages_of(before).contains(&page)) {
                self.ask(Request::Text(page));
            }
        }
    }

    /// Mark every glyph between the two ends of the drag and remember the text they spell.
    ///
    /// The two ends are put in document order first, so a drag pulled upwards reads the same way
    /// down as one pulled down. Each page in between contributes all of its glyphs, and the two
    /// at the ends contribute from or up to the glyph nearest the pointer.
    fn select(&self, span: Span) {
        let glyphs = self.glyphs.borrow();
        let end_of = |(page, at): (usize, (f32, f32))| {
            glyphs
                .get(&page)
                .and_then(|g| Some((page, nearest(g, at)?)))
        };
        let (Some(a), Some(b)) = (end_of(span.from), end_of(span.to)) else {
            return;
        };
        let (start, end) = (a.min(b), a.max(b));
        let mut text = String::new();
        let mut boxes = Vec::new();
        let mut ranges = Vec::new();
        for page in start.0..=end.0 {
            let Some(page_glyphs) = glyphs.get(&page) else {
                return;
            };
            let lo = match page == start.0 {
                true => start.1,
                false => 0,
            };
            let hi = match page == end.0 {
                true => end.1,
                false => page_glyphs.len().saturating_sub(1),
            };
            let Some(picked) = page_glyphs.get(lo..=hi) else {
                continue;
            };
            // A page break reads as a line break, which is what pasting a passage that runs over
            // one should give.
            if !text.is_empty() {
                text.push('\n');
            }
            text.extend(picked.iter().map(|g| g.ch));
            // The indices as well as the boxes: a link is made of the numbers, and a rectangle
            // on screen cannot be turned back into one.
            ranges.push(pdf::Selection {
                page,
                start: lo,
                end: hi + 1,
            });
            // A glyph with no box of its own — a space between words — would paint as a dot.
            boxes.push((
                page,
                picked
                    .iter()
                    .map(|g| g.rect)
                    .filter(|r| r.width() > 0.0 && r.height() > 0.0)
                    .collect(),
            ));
        }
        *self.selected.borrow_mut() = text;
        *self.ranges.borrow_mut() = ranges;
        self.view.set_selection(boxes);
    }

    /// Drop the selection, on a click that is not a drag.
    fn clear_selection(&self) {
        if self.selected.borrow().is_empty() {
            return;
        }
        self.selected.borrow_mut().clear();
        self.ranges.borrow_mut().clear();
        self.view.set_selection(Vec::new());
    }

    /// A click on the page: follow a link if there is one under it.
    /// A click that was not a drag: open the note whose link paints a highlight here.
    ///
    /// After the link handler, which answers on the press — a link inside a highlight is still a
    /// link, and following it is what a click on one has always meant.
    fn clicked_highlight(self: &Rc<Self>, view: &PdfView, x: f64, y: f64) {
        if self.link_at(view, x, y).is_some() {
            return;
        }
        let Some(at) = view.highlight_at(x, y) else {
            return;
        };
        let note = self
            .notes
            .borrow()
            .get(at)
            .map(|l| (l.src_rel_path.clone(), l.byte_start.max(0) as usize));
        let hook = self.on_note.borrow().clone();
        if let (Some((rel, byte)), Some(f)) = (note, hook) {
            f(&rel, byte);
        }
    }

    fn click(self: &Rc<Self>, view: &PdfView, x: f64, y: f64) {
        self.clear_selection();
        let Some(target) = self.link_at(view, x, y) else {
            return;
        };
        match target {
            LinkTarget::Page { page, top } => {
                self.jumping();
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
                // than making the user drag again. A drag across a page break waits for the last
                // page it covers: `select` needs all of them to know where the middle ones end.
                let waiting = self
                    .pending_select
                    .get()
                    .filter(|span| pages_of(*span).contains(&page));
                if let Some(span) = waiting {
                    let have = self.glyphs.borrow();
                    if pages_of(span).all(|at| have.contains_key(&at)) {
                        drop(have);
                        self.pending_select.set(None);
                        self.select(span);
                    }
                }
                // Or a link is waiting for this page, which is Follow Link into the document.
                if self.pending_show.get().is_some_and(|(at, _)| at == page) {
                    self.apply_show();
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
            Reply::Highlights(map) => self.view.set_highlights(map),
            Reply::Exported(result) => {
                let hook = self.on_export.borrow().clone();
                if let Some(f) = hook {
                    f(self, result);
                }
            }
            Reply::PageChanged(page) => {
                // The reading view keeps painting what it has until the new render arrives; the
                // strip has only a stand-in, which `refresh_page` drops, so it asks for another.
                self.view.refresh_page(page);
                self.thumbs.queue_draw();
                self.save_soon();
            }
            Reply::Saved(etag) => self.saved.set(Some(etag)),
            Reply::Reloaded(sizes) => {
                // The anchor is taken now rather than when the reload was asked for: the reader
                // may have moved while the file was being re-read.
                let anchor = self.view.anchor().clamped(sizes.len());
                self.view.forget_textures();
                self.thumbs.forget_textures();
                self.links.borrow_mut().clear();
                // The glyphs and anything made of them are of the old document: an export or a
                // rebuild moves the text, and a stale index would paint the selection elsewhere.
                self.glyphs.borrow_mut().clear();
                self.clear_selection();
                self.view.set_sizes(sizes.clone());
                self.thumbs.set_sizes(sizes);
                match self.pending.take() {
                    // The document just opened: go where the session left the reader.
                    Some(place) => self.view.goto_page(place.page, None),
                    None => self.view.scroll_to(anchor),
                }
                self.ask(Request::Outline);
                // A link followed into a document that was still opening waits here.
                if let Some((page, sel)) = self.pending_show.get() {
                    self.show_link(page, sel);
                }
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

/// Put the selected text into a link as its alias: `[[f.pdf#page=1&selection=…|the text]]`.
///
/// The alias is what a reader sees in the note and what re-anchors the highlight when the
/// selection numbers no longer fit the document, so it is the text and not a label. Newlines
/// collapse — a link is one line — and the three characters that would end the link early are
/// dropped rather than escaped, because a wikilink has no escape for them.
fn link_with_alias(link: &str, text: &str) -> String {
    let alias: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(['|', '[', ']'], "");
    match alias.is_empty() {
        true => link.to_string(),
        false => format!("{}|{alias}]]", link.trim_end_matches("]]")),
    }
}

/// Where each note link lands on the page today, per page, with the index of the link it is.
///
/// The four numbers first, and the text the link quotes as the fallback — a document rebuilt
/// with different line breaks moves the numbers but not the sentence. A link whose quads a real
/// `/Highlight` already covers is left out: it has been exported, and the annotation is in the
/// page's own pixels.
fn highlight_quads(doc: &PdfDoc, links: &[PdfLink]) -> pdfview::Highlights {
    let mut glyphs: HashMap<usize, Vec<pdf::Glyph>> = HashMap::new();
    let mut existing: HashMap<usize, Vec<pdf::Highlight>> = HashMap::new();
    let mut out = pdfview::Highlights::new();
    for (at, link) in links.iter().enumerate() {
        let page = link.page;
        let found = glyphs
            .entry(page)
            .or_insert_with(|| doc.page_text(page).unwrap_or_default());
        let quads = pdf::selection_quads(found, link.selection)
            .map(|(_, quads)| quads)
            .or_else(|| {
                let text = link.alias.as_deref()?;
                doc.search(page, text).ok()?.into_iter().next()
            });
        let Some(quads) = quads.filter(|q| !q.is_empty()) else {
            continue;
        };
        let already = existing
            .entry(page)
            .or_insert_with(|| doc.highlights_on(page).unwrap_or_default());
        if already.iter().any(|h| pdf::same_quads(&h.quads, &quads)) {
            continue;
        }
        out.entry(page).or_default().push((quads, at));
    }
    out
}

/// Every page a drag covers, in document order however the drag was pulled.
fn pages_of(span: Span) -> std::ops::RangeInclusive<usize> {
    span.from.0.min(span.to.0)..=span.from.0.max(span.to.0)
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
/// One request at a time, with one twist: before every tile and every searched page it drains the
/// queue, so a batch that has been overtaken is put aside rather than finished into a viewport
/// nobody is looking at any more. Only a request of the same kind abandons it — see
/// [`interrupt`] — and the queue is a stack, so the newest work is always what runs next and
/// what is put aside resumes after it.
///
/// Dropping an interrupted batch instead loses it for good. Nothing re-asks: the widget sends a
/// list of tiles again only when that list changes, and the tab sends a query again only when the
/// text does, so a tile nobody rendered stayed blurry and a search pushed aside reported the
/// matches of the pages it had reached and no more.
fn render_loop(
    mut doc: PdfDoc,
    path: PathBuf,
    rx: std::sync::mpsc::Receiver<Request>,
    view: glib::SendWeakRef<PdfView>,
) {
    // What the file looked like when this document was read. Every write from here updates it,
    // which is how the tab tells its own save from someone else's and does not reload over it.
    let mut etag = accent_core::fs::Etag::of(&path).ok();
    // What has been drawn here, so Undo reaches this session's strokes and no others.
    let mut ink = Ink::default();
    // The channel closing is the tab going away, which is the only way this thread ends.
    while let Ok(first) = rx.recv() {
        let mut queue = vec![first];
        while let Some(current) = queue.pop() {
            match current {
                Request::Tiles {
                    scale,
                    dark,
                    theme,
                    wants,
                } => {
                    let mut at = 0;
                    while at < wants.len() {
                        match rx.try_recv() {
                            Ok(newer) => {
                                let rest = Request::Tiles {
                                    scale,
                                    dark,
                                    theme,
                                    wants: wants[at..].to_vec(),
                                };
                                interrupt(&mut queue, rest, newer);
                                break;
                            }
                            Err(TryRecvError::Disconnected) => return,
                            Err(TryRecvError::Empty) => {}
                        }
                        render_want(&doc, &view, scale, dark, theme, wants[at]);
                        at += 1;
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
                Request::Search { query, text, from } => {
                    let pages = doc.page_count();
                    let mut at = from;
                    while at < pages {
                        // Between pages, and no finer: pdfium loads a page's text whole
                        // (`FPDFText_LoadPage`) and the search cursor runs over that, so one page
                        // is the smallest unit there is to stop at. Measured on a 500-page A4
                        // document, that is 0.5 ms typical and 1.6 ms at worst — well inside a
                        // frame, so a tile asked for mid-query waits no longer than that.
                        match rx.try_recv() {
                            Ok(newer) => {
                                let rest = Request::Search {
                                    query,
                                    text,
                                    from: at,
                                };
                                interrupt(&mut queue, rest, newer);
                                break;
                            }
                            Err(TryRecvError::Disconnected) => return,
                            Err(TryRecvError::Empty) => {}
                        }
                        match doc.search(at, &text) {
                            Ok(hits) if !hits.is_empty() => send(
                                &view,
                                Reply::Found {
                                    query,
                                    page: at,
                                    hits,
                                },
                            ),
                            _ => {}
                        }
                        at += 1;
                    }
                }
                Request::Ink {
                    page,
                    points,
                    style,
                } => {
                    let before = doc.annotation_count(page).unwrap_or(0);
                    match doc.add_ink(page, &points, style) {
                        Ok(()) => {
                            ink.note(page, before);
                            ink.added.push(page);
                            ink.dirty = true;
                            send(&view, Reply::PageChanged(page));
                        }
                        Err(e) => tracing::warn!("drawing on page {page}: {e:#}"),
                    }
                }
                Request::Erase { page, at } => {
                    let hit = doc.ink_paths(page).unwrap_or_default();
                    let found = hit
                        .iter()
                        .find(|(_, points)| pdf::hit(points, at, ERASE_RADIUS));
                    if let Some((index, _)) = found {
                        let before = doc.annotation_count(page).unwrap_or(0);
                        ink.note(page, before);
                        if let Err(e) = doc.delete_annotation(page, *index) {
                            tracing::warn!("erasing on page {page}: {e:#}");
                            continue;
                        }
                        ink.erased(page, *index);
                        ink.dirty = true;
                        send(&view, Reply::PageChanged(page));
                    }
                }
                Request::Undo => {
                    while let Some(page) = ink.added.pop() {
                        let count = doc.annotation_count(page).unwrap_or(0);
                        let Some(index) = ink.undo_target(page, count) else {
                            continue;
                        };
                        match doc.delete_annotation(page, index) {
                            Ok(()) => {
                                ink.dirty = true;
                                send(&view, Reply::PageChanged(page));
                            }
                            Err(e) => tracing::warn!("undoing on page {page}: {e:#}"),
                        }
                        break;
                    }
                }
                Request::Save(ack) => {
                    if !ink.dirty {
                        // The ack still goes: a caller waiting on it is waiting for the file to
                        // be right, and it already is.
                        drop(ack);
                        continue;
                    }
                    // ponytail: `save_to_bytes` rewrites the whole file under the pdfium lock, so
                    // a very large PDF stops the tiles for as long as that takes. Saving
                    // incrementally is the upgrade.
                    match doc.save().map_err(|e| e.to_string()).and_then(|bytes| {
                        accent_core::fs::write_bytes(&path, &bytes, etag).map_err(|e| e.to_string())
                    }) {
                        Ok(written) => {
                            etag = Some(written);
                            ink.dirty = false;
                            send(&view, Reply::Saved(written));
                        }
                        // Left dirty on purpose: the next stroke's save tries again, and the
                        // drawing is still in the document either way.
                        Err(e) => tracing::warn!("saving {}: {e}", path.display()),
                    }
                    // Dropping the sender is the signal: the receiver's `recv` returns either
                    // way, so a failed save does not hang the window that is closing.
                    drop(ack);
                }
                Request::Highlights(links) => {
                    send(&view, Reply::Highlights(highlight_quads(&doc, &links)));
                }
                Request::Export { links, color } => {
                    let quads = highlight_quads(&doc, &links);
                    let pages: Vec<usize> = quads.keys().copied().collect();
                    let highlights: Vec<pdf::Highlight> = quads
                        .into_iter()
                        .flat_map(|(page, found)| {
                            found.into_iter().map(move |(quads, at)| (page, quads, at))
                        })
                        .map(|(page, quads, at)| pdf::Highlight {
                            page,
                            quads,
                            color: [color[0], color[1], color[2], 255],
                            contents: links.get(at).and_then(|l| l.alias.clone()),
                        })
                        .collect();
                    let written = doc
                        .add_highlights(&highlights)
                        .and_then(|added| match added {
                            // Nothing new is not a write: the file is already what it should be.
                            0 => Ok(0),
                            _ => {
                                let bytes = doc.save()?;
                                let written = accent_core::fs::write_bytes(&path, &bytes, etag)?;
                                etag = Some(written);
                                send(&view, Reply::Saved(written));
                                for page in pages {
                                    send(&view, Reply::PageChanged(page));
                                }
                                Ok(added)
                            }
                        })
                        .map_err(|e| format!("{e:#}"));
                    send(&view, Reply::Exported(written));
                }
                Request::Reload => {
                    // Swapped only on success: a half-written PDF fails to open often while a
                    // LaTeX run is going, and the next event tries again.
                    match PdfDoc::open(&path) {
                        Ok(fresh) => {
                            doc = fresh;
                            etag = accent_core::fs::Etag::of(&path).ok();
                            send(&view, Reply::Reloaded(page_sizes(&doc)));
                        }
                        Err(e) => tracing::debug!("reloading {}: {e:#}", path.display()),
                    }
                }
            }
        }
    }
}

/// Put `newer` at the top of the queue, and `rest` — what the interrupted batch has left to do —
/// under it or not at all.
///
/// Only a request of the same kind takes a batch over: a newer viewport makes the old tiles
/// pointless, and a newer query makes the old query's remaining pages pointless. It also drops
/// any older remainder of that kind still waiting further down, which is the one a batch put
/// aside earlier left there. Anything else — a link, a page's glyphs, an outline, a reload — is a
/// short detour, and the batch resumes once it is done.
fn interrupt(queue: &mut Vec<Request>, rest: Request, newer: Request) {
    match same_kind(&rest, &newer) {
        false => queue.push(rest),
        true => queue.retain(|waiting| !same_kind(waiting, &newer)),
    }
    queue.push(newer);
}

/// Whether two requests are the same kind of work, whatever they are for.
fn same_kind(a: &Request, b: &Request) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
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
    use super::{BAND, Ink, Request, band_offset, interrupt, link_with_alias};

    /// Undo takes back this session's strokes and stops at whatever the document already had,
    /// however the eraser moved the line in between.
    #[test]
    fn undo_never_reaches_a_pre_existing_annotation() {
        let mut ink = Ink::default();
        // A page that already carried three annotations, then two strokes of ours.
        ink.note(0, 3);
        ink.added.push(0);
        ink.added.push(0);
        assert_eq!(ink.undo_target(0, 5), Some(4));

        // The reader erases one of the document's own: the line moves down, ours are still ours.
        ink.erased(0, 1);
        assert_eq!(ink.undo_target(0, 4), Some(3));
        assert_eq!(ink.added.len(), 2);

        // Erasing one of ours leaves one stroke to undo, and then nothing.
        ink.erased(0, 3);
        assert_eq!(ink.added.len(), 1);
        assert_eq!(ink.undo_target(0, 3), Some(2));
        assert_eq!(
            ink.undo_target(0, 2),
            None,
            "the document's own are not ours"
        );

        // A page never drawn on has nothing to undo, whatever it carries.
        assert_eq!(ink.undo_target(9, 7), None);
    }

    #[test]
    fn link_with_alias_strips_what_would_end_the_link() {
        let link = "[[a.pdf#page=1&selection=0,0,0,5]]";
        assert_eq!(
            link_with_alias(link, " a |b]]\n c "),
            "[[a.pdf#page=1&selection=0,0,0,5|a b c]]"
        );
        // Nothing worth quoting is no alias, not an empty one.
        assert_eq!(link_with_alias(link, "  \n "), link);
    }

    fn search(query: u64, from: usize) -> Request {
        Request::Search {
            query,
            text: "q".to_string(),
            from,
        }
    }

    /// A batch pushed aside by a detour comes back; one pushed aside by its own kind does not,
    /// and takes any older remainder of that kind with it.
    #[test]
    fn only_the_same_kind_of_request_abandons_a_batch() {
        let mut queue = vec![search(1, 40)];
        // A page's glyphs are a detour: the query that was running resumes after them.
        interrupt(&mut queue, search(2, 10), Request::Text(3));
        assert!(matches!(queue.pop(), Some(Request::Text(3))));
        assert!(matches!(
            queue.pop(),
            Some(Request::Search {
                query: 2,
                from: 10,
                ..
            })
        ));
        // A newer query replaces the one running and the older one still waiting under it.
        let mut queue = vec![search(1, 40), Request::Outline];
        interrupt(&mut queue, search(2, 10), search(3, 0));
        assert!(matches!(
            queue.pop(),
            Some(Request::Search {
                query: 3,
                from: 0,
                ..
            })
        ));
        assert!(matches!(queue.pop(), Some(Request::Outline)));
        assert!(queue.is_empty());
    }

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
