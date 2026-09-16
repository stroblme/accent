//! A PDF in a tab: the document, the reading state around it, and the hooks the window listens
//! on.
//!
//! The widget in `view.rs` knows nothing about pdfium; it asks for tiles and paints what it is
//! given. `render.rs` is the other half, one thread per open document; this is the reader
//! between them — the history, the search and the outline that make it more than a viewer.

use super::protocol::Request;
use super::ring;
use super::selection::pages_of;
use super::{self as pdfview, Anchor, PdfView, PdfZoom, Reply, Span, render};
use accent_api::PdfLink;
use accent_core::pdf;
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{Sender, channel};

/// How long after the last stroke the document is written out.
const INK_SAVE: std::time::Duration = std::time::Duration::from_secs(1);

/// Where a document is being read, remembered per file in the session.
pub use accent_core::config::PdfPlace as Place;

pub(super) type Hook = RefCell<Option<Rc<dyn Fn(&Rc<PdfTab>)>>>;
/// A highlight was clicked: the note holding the link, and the byte it starts at.
type NoteHook = RefCell<Option<Rc<dyn Fn(&str, usize)>>>;
/// An export finished, with what it wrote or why it could not.
type ExportHook = RefCell<Option<Rc<dyn Fn(&Rc<PdfTab>, Result<usize, String>)>>>;
/// The drawing could not be written, and why.
type FailHook = RefCell<Option<Rc<dyn Fn(&Rc<PdfTab>, String)>>>;
/// A width or a colour was picked on the ring for a tool.
type ChoiceHook = RefCell<Option<Rc<dyn Fn(&Rc<PdfTab>, pdfview::Mode, ring::Choice)>>>;
type UriHook = RefCell<Option<Rc<dyn Fn(&str)>>>;

pub struct PdfTab {
    pub(super) key: RefCell<String>,
    pub(super) path: RefCell<PathBuf>,
    pub page: adw::TabPage,
    /// "view" once a document is open, "status" when there is nothing to show and a reason why.
    pub(super) stack: gtk::Stack,
    pub(super) status: adw::StatusPage,
    pub(super) view: PdfView,
    pub(super) thumbs: PdfView,
    /// A plain box between the stack and the scroller, and the link preview's parent: a popover
    /// hung off a widget with a `size_allocate` of its own never re-presents (DESIGN.md, States),
    /// and `PdfView` has one.
    pub(super) host: gtk::Box,
    /// The drawing tools, floating over the page while the window says they are wanted.
    pub(super) ring: Rc<ring::PdfRing>,
    /// The strip the thumbnails live in, built once. Handing the Outline pane a fresh
    /// `GtkScrolledWindow` around the same widget every time would re-parent a widget that
    /// already has a parent, which GTK refuses with a critical.
    pub(super) thumb_strip: gtk::ScrolledWindow,
    /// Requests to the render thread. Dropping it is what ends the thread, so it is dropped with
    /// the tab and nothing else has to be joined.
    pub(super) tx: RefCell<Option<Sender<Request>>>,
    /// Colours inverted against the system's choice, for a document that renders badly either way.
    pub(super) inverted: Cell<bool>,
    /// The palette the cached tiles were rendered in, so a theme change can tell that they are
    /// of the old one. See [`PdfTab::restyle`].
    pub(super) theme: Cell<pdf::Theme>,
    /// The document could not be opened, so it is not opening either.
    pub(super) failed: Cell<bool>,
    /// The zoom to restore when presentation mode ends.
    pub(super) presenting: Cell<Option<PdfZoom>>,
    pub(super) links: RefCell<std::collections::HashMap<usize, Vec<pdf::Link>>>,
    /// Each page's glyphs, fetched the first time someone drags across that page.
    pub(super) glyphs: RefCell<std::collections::HashMap<usize, Vec<pdf::Glyph>>>,
    /// The selected text, for Ctrl+C.
    pub(super) selected: RefCell<String>,
    /// The same selection as glyph ranges, one per page it covers, which is what a link is made
    /// of. Kept beside the text because the boxes on screen cannot be turned back into indices.
    pub(super) ranges: RefCell<Vec<pdf::Selection>>,
    /// The note links that highlight this document, as the index last reported them. The painted
    /// quads carry an index into this, so a click on one knows which note to open.
    pub(super) notes: RefCell<Vec<PdfLink>>,
    /// A page and selection to show once the glyphs for it arrive: Follow Link into a PDF.
    pub(super) pending_show: Cell<Option<(usize, Option<[usize; 4]>)>>,
    /// A save is already scheduled, so a burst of strokes costs one write.
    pub(super) save_pending: Cell<bool>,
    /// A write of this document is on its way somewhere else — the ssh upload of a remote
    /// vault's copy — and whether another save landed while it was. See [`PdfTab::claim_upload`].
    pub(super) uploading: Cell<bool>,
    pub(super) upload_again: Cell<bool>,
    /// The etag of the last write *this tab* made, so a watcher report of our own save is
    /// recognised and not answered with a reload. See [`PdfTab::refresh`].
    pub(super) saved: Cell<Option<accent_core::fs::Etag>>,
    /// Whether Undo, and then Redo, has anything to walk: what the header's two buttons show.
    pub(super) history: Cell<(bool, bool)>,
    pub(super) outline: RefCell<Vec<pdf::Outline>>,
    /// The link preview on screen, if the pointer is on a link with Ctrl held.
    pub(super) preview: RefCell<Option<super::preview::Preview>>,
    /// A drag that arrived before the glyphs of every page it covers did, to answer when the
    /// last of them lands.
    pub(super) pending_select: Cell<Option<Span>>,
    /// Where the session says this document was left, until the first page sizes arrive and it
    /// can be applied. `None` afterwards, so a reload keeps the reader where they are instead.
    pub(super) pending: Cell<Option<Place>>,
    /// Which search these results belong to, so a stale page's answer is dropped.
    pub(super) query: Cell<u64>,
    pub(super) matches: RefCell<Vec<(usize, pdf::Rect)>>,
    pub(super) current: Cell<Option<usize>>,
    pub(super) on_zoom: Hook,
    pub(super) on_page: Hook,
    /// Fired just before a jump, so the pane can record where the reader was.
    pub(super) on_jump: Hook,
    pub(super) on_outline: Hook,
    /// Fired when the document's pages are known, which is when it stops being "opening".
    pub(super) on_open: Hook,
    pub(super) on_matches: Hook,
    pub(super) on_uri: UriHook,
    pub(super) on_mode: Hook,
    pub(super) on_history: Hook,
    pub(super) on_note: NoteHook,
    pub(super) on_export: ExportHook,
    pub(super) on_save_failed: FailHook,
    /// Fired once the file on this machine holds what was drawn, for a vault whose real copy is
    /// somewhere else.
    pub(super) on_saved: Hook,
    pub(super) on_choice: ChoiceHook,
}

/// A new tab of `tabs` for the PDF at `path`, showing itself opening until [`PdfTab::load`] is
/// handed the bytes. Never fails: a document that will not open is a tab holding the reason.
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
    let ring = ring::PdfRing::new();
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
        uploading: Cell::new(false),
        upload_again: Cell::new(false),
        saved: Cell::new(None),
        history: Cell::new((false, false)),
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
        on_history: RefCell::new(None),
        on_note: RefCell::new(None),
        on_export: RefCell::new(None),
        on_save_failed: RefCell::new(None),
        on_saved: RefCell::new(None),
        on_choice: RefCell::new(None),
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
    tab.wire_strip(&thumbs);
    tab.ring.connect_choice(glib::clone!(
        #[weak]
        tab,
        move |tool, choice| {
            let hook = tab.on_choice.borrow().clone();
            if let Some(f) = hook {
                f(&tab, tool, choice);
            }
        }
    ));
    tab.wire_keys();
    tab.wire_preview();
    tab.wire_menu();

    // Nothing about the document is known yet, and deliberately so: opening it and measuring its
    // pages is pdfium work, which for a thousand-page file is most of a second, and on a remote
    // vault the bytes are still to be fetched. The tab goes up empty and fills in when the render
    // thread reports back, so the window is on screen in the time it takes to build a widget.
    tab.stack.set_visible_child_name("view");
    tab
}

impl PdfTab {
    pub fn key(&self) -> String {
        self.key.borrow().clone()
    }

    pub fn path(&self) -> PathBuf {
        self.path.borrow().clone()
    }

    /// Read the document from `path`, a file on this machine: the vault's own, or a remote
    /// vault's copy once the fetch has landed.
    pub fn load(self: &Rc<Self>, path: &Path) {
        *self.path.borrow_mut() = path.to_path_buf();
        if let Err(message) = self.start() {
            self.fail(&message);
        }
    }

    pub fn page_count(&self) -> usize {
        self.view.page_count()
    }

    /// The widget a reader's keys have to reach: the reading view, which is where [`Self::wire_keys`]
    /// puts them. What a window hands the keyboard to when it moves this tab into another pane.
    pub fn key_target(&self) -> gtk::Widget {
        self.view.clone().upcast()
    }

    pub fn place(&self) -> Place {
        Place {
            page: self.view.current_page(),
            zoom: self.view.zoom(),
        }
    }

    /// A rename landed: follow the file without losing where the reader is.
    pub fn retarget(&self, root: &Path, key: &str) {
        let moved = tree_of(&self.path(), &self.key())
            .unwrap_or_else(|| root.to_path_buf())
            .join(key);
        *self.key.borrow_mut() = key.to_string();
        *self.path.borrow_mut() = moved.clone();
        self.page.set_title(crate::doc::file_name(key));
        self.page
            .set_tooltip(&crate::fileops::display_path(root, key));
        // The render thread owns the document and the path it reads and writes; without this it
        // keeps the old name, a reload re-opens a file that is gone, and the pen's next save
        // fails with "file vanished before save".
        self.ask(Request::Retarget(moved));
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

    /// Page from a key, through `win.pdf-next-page` / `win.pdf-previous-page`.
    ///
    /// The keys are dispatched here rather than from the window's accelerator table — a bare
    /// `space` or arrow there would be taken from every entry in the app — but the *command* is
    /// the window's, so the palette lists it and a menu or a script can fire it. This is the one
    /// place the keys and the palette meet.
    fn page(&self, forward: bool) {
        self.run(match forward {
            true => "win.pdf-next-page",
            false => "win.pdf-previous-page",
        });
    }

    /// Fire one of the window's commands from the page. What a key over a PDF does is a command
    /// like any other, so it lists in the palette and can be rebound.
    fn run(&self, action: &str) {
        let _ = self.view.activate_action(action, None);
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

    /// The note links that highlight this document, as the index reports them. Painting them
    /// needs the pages' glyphs, so the render thread answers.
    pub fn set_note_links(self: &Rc<Self>, links: Vec<PdfLink>) {
        *self.notes.borrow_mut() = links.clone();
        self.ask(Request::Highlights(links));
    }

    /// Whether any note links into this document, which is whether there is anything to export.
    pub fn has_note_links(&self) -> bool {
        !self.notes.borrow().is_empty()
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
    pub(super) fn apply_show(&self) {
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
        if self.wants_inks() {
            self.ask_inks();
        }
        self.emit(&self.on_mode);
    }

    /// Whether the tool in hand needs to know what is drawn on the page, which every tool does:
    /// the Adjust tool takes hold of a stroke, the eraser has to find the one under the pointer,
    /// and a stylus's eraser tip erases under any of them.
    fn wants_inks(&self) -> bool {
        self.view.mode() != pdfview::Mode::Select
    }

    /// Give those tools the page under the reader and its neighbours.
    ///
    // ponytail: three pages, so a fourth that is partly on screen waits until it is current.
    fn ask_inks(&self) {
        let page = self.view.current_page();
        let last = self.view.page_count().saturating_sub(1);
        for p in page.saturating_sub(1)..=(page + 1).min(last) {
            self.ask(Request::Inks(p));
        }
    }

    /// What the preferences say about the tools.
    pub fn set_drawing_config(&self, config: accent_core::config::DrawingConfig) {
        self.ring.set_config(&config);
        self.view.set_drawing_config(config);
    }

    /// Show or hide the ring of tools.
    pub fn set_drawing(&self, showing: bool, at: Option<(f64, f64)>) {
        self.ring.set_visible(showing, at);
    }

    /// Where the reader has dragged the ring, so the next tab to show one puts it there.
    pub fn ring_at(&self) -> Option<(f64, f64)> {
        self.ring.at()
    }

    /// What the status bar says while a pen is out, or nothing while one is not. The action's
    /// own label, so the readout, the ring's tooltip and the palette say one word between them.
    pub fn mode_label(&self) -> Option<&'static str> {
        self.view.mode().action().map(crate::actions::label_of)
    }

    /// Take back the last stroke drawn, drag erased or move made in this tab, through
    /// `win.pdf-undo`.
    pub fn undo(self: &Rc<Self>) {
        self.ask(Request::Undo);
    }

    /// Make again what Undo last took back, through `win.pdf-redo`.
    pub fn redo(self: &Rc<Self>) {
        self.ask(Request::Redo);
    }

    /// Whether Undo, and then Redo, has anything to walk, as the render thread last said.
    pub fn history(&self) -> (bool, bool) {
        self.history.get()
    }

    /// Called when what Undo and Redo can reach changes.
    pub fn connect_history(&self, f: impl Fn(&Rc<PdfTab>) + 'static) {
        *self.on_history.borrow_mut() = Some(Rc::new(f));
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

    /// Another blank page at the end, the size of the last one — a notebook's answer to running
    /// out of paper. Written out on the same timer a stroke is: the thread marks the document
    /// dirty and this asks for the save a second later.
    pub fn add_page(self: &Rc<Self>) {
        self.ask(Request::AddPage);
        self.save_soon();
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
    pub fn connect_choice(&self, f: impl Fn(&Rc<PdfTab>, pdfview::Mode, ring::Choice) + 'static) {
        *self.on_choice.borrow_mut() = Some(Rc::new(f));
    }

    pub fn connect_export(&self, f: impl Fn(&Rc<PdfTab>, Result<usize, String>) + 'static) {
        *self.on_export.borrow_mut() = Some(Rc::new(f));
    }

    /// Called when a drawing could not be written out, with the reason to say.
    pub fn connect_save_failed(&self, f: impl Fn(&Rc<PdfTab>, String) + 'static) {
        *self.on_save_failed.borrow_mut() = Some(Rc::new(f));
    }

    /// Called once a write has landed in the file this tab reads, which on a remote vault is the
    /// cached copy and not the document itself.
    pub fn connect_saved(&self, f: impl Fn(&Rc<PdfTab>) + 'static) {
        *self.on_saved.borrow_mut() = Some(Rc::new(f));
    }

    /// Take the right to start sending the written-out file somewhere. `false` when one is
    /// already on its way: that one is marked to go again rather than a second starting beside
    /// it, so a burst of strokes costs two transfers and the far end is never more than one
    /// behind.
    pub fn claim_upload(&self) -> bool {
        if self.uploading.replace(true) {
            self.upload_again.set(true);
            return false;
        }
        true
    }

    /// The transfer came back: `true` when a save landed while it was out and the file has to go
    /// once more.
    pub fn upload_done(&self) -> bool {
        self.uploading.set(false);
        self.upload_again.replace(false)
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
        // Sent even when there is nothing to look for: a query of the same kind takes over from
        // the one running, so this is what stops a search the reader has cleared or closed the
        // bar on, which used to finish the whole document into replies nobody reads.
        self.ask(Request::Search {
            query: self.query.get(),
            text: text.to_string(),
            from: 0,
        });
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

    pub(super) fn emit(self: &Rc<Self>, hook: &Hook) {
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

    /// The document will not open, whether the render thread could not start or could not read
    /// it: say why in place of the pages, and tell the window, which is still showing it as
    /// opening.
    fn fail(self: &Rc<Self>, message: &str) {
        self.failed.set(true);
        self.show_status(message);
        self.emit(&self.on_open);
    }

    pub(super) fn ask(&self, request: Request) {
        let tx = self.tx.borrow();
        if let Some(tx) = tx.as_ref() {
            let _ = tx.send(request);
        }
    }

    /// About to jump: the window records where the reader is, so Back returns here. Fired before
    /// the view moves, which is what makes `anchor()` still the place being left.
    pub(super) fn jumping(self: &Rc<Self>) {
        self.emit(&self.on_jump);
    }
}

impl PdfTab {
    /// Start the render thread, which opens the document and then answers requests for it.
    fn start(self: &Rc<Self>) -> Result<(), String> {
        let weak = glib::SendWeakRef::from(self.view.downgrade());
        *self.tx.borrow_mut() = Some(render::spawn(self.path(), weak)?);
        Ok(())
    }

    /// Hook up one of the two views: what it wants rendered, and what comes back.
    fn wire(self: &Rc<Self>, view: &PdfView) {
        self.wire_strip(view);
        view.connect_reply(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_, reply| tab.on_reply(reply)
        ));
        // A page's stand-in landed. It is all the strip paints of a page, and the strip hears of
        // it only here: what it asks for is answered to the reading view, which repaints itself.
        view.connect_lowres(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |page| {
                tab.thumbs.queue_draw();
                tab.band_landed(page);
            }
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
                if tab.wants_inks() {
                    tab.ask_inks();
                }
                tab.emit(&tab.on_page);
            }
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
        view.connect_ink(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |page, points| {
                let mode = tab.view.mode();
                let style = tab.view.ink_style(mode);
                match (mode.shapes(), points.as_slice()) {
                    (true, &[a, b]) => {
                        if let Some(shape) = pdfview::shape_of(mode, a, b) {
                            tab.ask(Request::Shape { page, shape, style });
                        }
                    }
                    (true, _) => {}
                    (false, _) => tab.ask(Request::Ink {
                        page,
                        points,
                        style,
                    }),
                }
            }
        ));
        view.connect_erase(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |page, id, partial, joined| tab.ask(Request::Erase {
                page,
                id,
                joined,
                partial
            })
        ));
        view.connect_transform(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |page, id, matrix| tab.ask(Request::Transform { page, id, matrix })
        ));
    }

    /// What both views answer: the tiles they want rendered, and a click that names a page.
    ///
    /// The rest of [`Self::wire`] is the reading view's alone. The strip is a column of
    /// thumbnails, not a page being read: a drag across it used to select text in the reading
    /// view, the pointer over it wore an I-beam, and scrolling it asked for the links and the
    /// strokes of whatever page went past.
    fn wire_strip(self: &Rc<Self>, view: &PdfView) {
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
        view.connect_goto(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |page| tab.goto_page(page)
        ));
    }

    /// The page's own menu, on a secondary click over it.
    ///
    /// Copy and Copy Link to Selection when there is a selection, then Add Page and Export
    /// Highlights, which are about the document rather than about what is selected and so are
    /// always offered — a read-only or remote document says so in a toast rather than by hiding
    /// the row. The drawing tools are not here: they are the ring, which the header's Drawing
    /// button opens.
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
                clipboard.append(Some(crate::actions::label_of(action)), Some(action));
            }
            menu.append_section(None, &clipboard);
        }
        let file = gio::Menu::new();
        for action in ["win.pdf-add-page", "win.pdf-export-highlights"] {
            file.append(Some(crate::actions::label_of(action)), Some(action));
        }
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
        // bar's Fit Height. The idle runs once the click is over.
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
                // Through the window's actions, not past them: the key is on the tab because
                // `Ctrl+C` and `Ctrl+Z` belong to whatever has the keyboard, but what they do is
                // the command the palette and the menus name.
                if key == gtk::gdk::Key::c && state.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
                    tab.run("win.pdf-copy");
                    return glib::Propagation::Stop;
                }
                // The pen's own keys, on the tab like Copy: `Ctrl+Z` and `Escape` belong to
                // whatever has the keyboard, and here that is the page being drawn on. Redo is
                // `Ctrl+Shift+Z` or `Ctrl+Y`, the two a note's own undo answers to.
                if tab.mode() != pdfview::Mode::Select {
                    if state.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
                        let history = match (key.to_lower(), shift) {
                            (gtk::gdk::Key::z, false) => Some("win.pdf-undo"),
                            (gtk::gdk::Key::z, true) | (gtk::gdk::Key::y, false) => {
                                Some("win.pdf-redo")
                            }
                            _ => None,
                        };
                        if let Some(action) = history {
                            tab.run(action);
                            return glib::Propagation::Stop;
                        }
                    }
                    if key == gtk::gdk::Key::Escape {
                        tab.set_mode(pdfview::Mode::Select);
                        return glib::Propagation::Stop;
                    }
                }
                // `Space`, `n`, `p` and the arrows stay bare keys here rather than joining the
                // table: an application accelerator is dispatched at the window ahead of whatever
                // has the keyboard, so a bare `space` in it would stop every entry in the app
                // from taking one. Paging still goes through the window's own commands below.
                //
                // Alt+Left and Alt+Right are Back and Forward, and Ctrl with an arrow is the
                // scroller's own step: only the bare key reads the document.
                let bare = !state.intersects(
                    gtk::gdk::ModifierType::CONTROL_MASK
                        | gtk::gdk::ModifierType::ALT_MASK
                        | gtk::gdk::ModifierType::SUPER_MASK,
                );
                match key {
                    gtk::gdk::Key::space if shift => tab.page(false),
                    gtk::gdk::Key::space => tab.page(true),
                    gtk::gdk::Key::n => tab.page(true),
                    gtk::gdk::Key::p => tab.page(false),
                    // A page back and a page forth whatever the zoom: horizontal movement is
                    // Shift and the wheel, and one key cannot mean two things.
                    gtk::gdk::Key::Left if bare => tab.page(false),
                    gtk::gdk::Key::Right if bare => tab.page(true),
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
                let found: Vec<pdf::Rect> = hits
                    .iter()
                    .filter_map(|hit| hit.iter().copied().reduce(pdf::Rect::union))
                    .collect();
                if found.is_empty() {
                    return;
                }
                // Page order is the order a reader steps through them, and the thread walks the
                // document forwards — so this page's matches go after the ones already found,
                // which a binary search places without sorting the list again per reply.
                let mut matches = self.matches.borrow_mut();
                let at = matches.partition_point(|(seen, _)| *seen <= page);
                matches.splice(at..at, found.iter().map(|rect| (page, *rect)));
                drop(matches);
                self.view.add_marks(page, found);
                self.emit(&self.on_matches);
            }
            Reply::Highlights(map) => self.view.set_highlights(map),
            Reply::Exported(result) => {
                let hook = self.on_export.borrow().clone();
                if let Some(f) = hook {
                    f(self, result);
                }
            }
            Reply::PageChanged(page, area) => {
                // The reading view keeps painting what it has until the new render arrives; the
                // strip has only a stand-in, which `refresh_page` drops, so it asks for another.
                self.view.refresh_page(page, area);
                self.thumbs.queue_draw();
                if self.wants_inks() {
                    self.ask_inks();
                }
                self.save_soon();
            }
            Reply::Inks { page, inks, erases } => self.view.set_inks(page, inks, erases),
            Reply::History { undo, redo } => {
                self.history.set((undo, redo));
                self.emit(&self.on_history);
            }
            Reply::Saved(etag) => {
                self.saved.set(Some(etag));
                let hook = self.on_saved.borrow().clone();
                if let Some(f) = hook {
                    f(self);
                }
            }
            Reply::SaveFailed(why) => {
                let hook = self.on_save_failed.borrow().clone();
                if let Some(f) = hook {
                    f(self, why);
                }
            }
            Reply::Paged(sizes) => {
                let last = sizes.len().saturating_sub(1);
                self.view.set_sizes(sizes.clone());
                self.thumbs.set_sizes(sizes);
                self.thumbs.queue_draw();
                // Land on the new page: adding one is asking for somewhere to draw, and a jump
                // so the reader can come back with Back.
                self.goto_page(last);
            }
            Reply::Reloaded(sizes) => {
                // The anchor is taken now rather than when the reload was asked for: the reader
                // may have moved while the file was being re-read.
                let anchor = self.view.anchor().clamped(sizes.len());
                self.view.forget_textures();
                self.thumbs.forget_textures();
                self.view.clear_inks();
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
                // The strokes went with the old document, and a tool in hand needs this one's.
                if self.wants_inks() {
                    self.ask_inks();
                }
                // A link followed into a document that was still opening waits here.
                if let Some((page, sel)) = self.pending_show.get() {
                    self.show_link(page, sel);
                }
                self.emit(&self.on_open);
            }
            Reply::Failed(message) => self.fail(&message),
            // Textures never reach here; `PdfView::deliver` keeps those.
            Reply::Tile(..) | Reply::Lowres { .. } => {}
        }
    }
}

/// How the pages of a document are coloured under the theme in force.
///
/// Must be called on the main thread: `theme.rs` holds the chosen theme in thread-local state.
pub(super) fn theme_of(dark: bool) -> pdf::Theme {
    match crate::theme::pdf_colours(dark) {
        Some((paper, ink)) => pdf::Theme::Recolour { paper, ink },
        None => pdf::Theme::Plain,
    }
}

/// The tree `path` reads `key` out of: `path` with `key`'s own components taken off the end.
///
/// A PDF is always read from a file on this machine, which is the vault's own on a local vault
/// and the ssh cache's copy on a remote one. Both mirror the vault, so a rename moves the key on
/// the end of whichever tree this tab was opened from — `root.join(key)` would point a remote
/// tab at the host's path, where there is nothing here to read.
fn tree_of(path: &Path, key: &str) -> Option<PathBuf> {
    let mut tree = path.to_path_buf();
    for _ in Path::new(key).components() {
        if !tree.pop() {
            return None;
        }
    }
    Some(tree)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rename_moves_the_key_on_the_end_of_the_tree_the_tab_reads_from() {
        let tree = |path: &str, key: &str| tree_of(Path::new(path), key);
        // A local vault: the tree is the vault root.
        assert_eq!(
            tree("/vault/papers/a.pdf", "papers/a.pdf"),
            Some(PathBuf::from("/vault"))
        );
        // A remote one: the same key under the ssh cache's mirror of the vault.
        assert_eq!(
            tree("/cache/host/papers/a.pdf", "papers/a.pdf"),
            Some(PathBuf::from("/cache/host"))
        );
        // Nothing sensible to say when the key is longer than the path it was read from.
        assert_eq!(tree("/a.pdf", "deep/nest/a.pdf"), None);
    }
}
