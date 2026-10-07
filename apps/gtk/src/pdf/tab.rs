//! A PDF in a tab: the document, the reading state around it, and the hooks the window listens
//! on.
//!
//! The widget in `view.rs` knows nothing about pdfium; it asks for tiles and paints what it is
//! given. `render.rs` is the other half, one thread per open document; this is the reader
//! between them — the history, the search and the outline that make it more than a viewer.

use super::protocol::Request;
use super::ring;
use super::{self as pdfview, Anchor, PdfView, PdfZoom, Span, render};
use crate::widgets::{Debounce, Hook};
use accent_api::{KeptLink, PdfLink};
use accent_core::pdf;
use accent_core::search::Options;
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, channel};

/// The page commands, each on the page the page's own menu was opened on, or else the page being
/// read ([`PdfTab::command_page`]): that menu and the status bar's page count offer the same three.
pub const PAGE_ACTIONS: [&str; 3] = [
    "win.pdf-add-page-before",
    "win.pdf-add-page-after",
    "win.pdf-delete-page",
];

/// Where a document is being read, remembered per file in the session.
pub use accent_core::config::PdfPlace as Place;

pub(super) type TabHook = Hook<dyn Fn(&Rc<PdfTab>)>;
/// A highlight was clicked: the note holding the link, and the byte it starts at.
type NoteHook = Hook<dyn Fn(&str, usize)>;
/// An export finished, with what it wrote or why it could not.
type ExportHook = Hook<dyn Fn(&Rc<PdfTab>, Result<usize, String>)>;
/// The drawing could not be written, and why.
type FailHook = Hook<dyn Fn(&Rc<PdfTab>, String)>;
/// A width or a colour was picked on the ring for a tool.
type ChoiceHook = Hook<dyn Fn(&Rc<PdfTab>, pdfview::Mode, ring::Choice)>;
type UriHook = Hook<dyn Fn(&str)>;
/// The pages were edited, or an edit walked by Undo or Redo: the edit made, and its step.
type RepageHook = Hook<dyn Fn(&Rc<PdfTab>, pdf::PageEdit, u32)>;
/// Another PDF's pages went in: its name, and how many or why they could not.
type ImportHook = Hook<dyn Fn(&Rc<PdfTab>, &str, Result<usize, String>)>;

/// The notes' links following this document's page edits, which the window runs
/// (`App::relink`): one rewrite at a time, in the order the edits were made, since each reads
/// what the one before it wrote.
#[derive(Default)]
pub struct Relinks {
    /// The edits still to follow, oldest first, each with its history step.
    pub queue: VecDeque<(pdf::PageEdit, u32)>,
    /// A rewrite is out on a worker.
    pub running: bool,
    /// The links each delete left naming the page it took out, by its step: what the Undo that
    /// puts the page back keeps where they are.
    pub left: HashMap<u32, Vec<KeptLink>>,
}

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
    /// The strip with the page buttons and the drop bar floating over it, which is what the
    /// Outline pane is handed. Built once, for the same reason.
    pub(super) organize: super::organize::Organize,
    /// Requests to the render thread. Dropping it is what ends the thread, so it is dropped with
    /// the tab and nothing else has to be joined.
    pub(super) tx: RefCell<Option<Sender<Request>>>,
    /// The imports asked for before the render thread started, a PDF dropped onto a row whose tab
    /// is still opening, which it is handed once it starts.
    pub(super) waiting: RefCell<Vec<Request>>,
    /// Colours inverted against the system's choice, for a document that renders badly either way.
    pub(super) inverted: Cell<bool>,
    /// The palette the cached tiles were rendered in, so a theme change can tell that they are
    /// of the old one. See [`PdfTab::restyle`].
    pub(super) theme: Cell<pdf::Theme>,
    /// The document could not be opened, so it is not opening either: the tab waits for the
    /// file to change.
    pub(super) failed: Cell<bool>,
    /// Where the reader was when the file stopped opening, for when it opens again.
    pub(super) resume: Cell<Option<Anchor>>,
    /// When the file was last reported changed, and whether a reload is waiting for it to
    /// settle. See [`PdfTab::refresh`].
    pub(super) changed: Cell<std::time::Instant>,
    pub(super) reload_due: Cell<bool>,
    /// How many times the render thread has answered an open, either way. Only drills read it.
    #[cfg(feature = "bench")]
    pub(super) opens: Cell<u32>,
    /// The zoom to restore when presentation mode ends.
    pub(super) presenting: Cell<Option<PdfZoom>>,
    pub(super) links: RefCell<std::collections::HashMap<usize, Vec<pdf::Link>>>,
    /// What other readers wrote on each page, asked for with its links.
    pub(super) comments: RefCell<HashMap<usize, Vec<pdf::Comment>>>,
    /// The tooltip's content for the comments it last showed, kept while the pointer stays on
    /// them: GTK asks again on every motion, and a new widget each time would rebuild the tooltip.
    pub(super) tip: RefCell<Option<(Vec<pdf::Comment>, gtk::Widget)>>,
    /// The comments a click pinned, in a popover whose text can be selected.
    pub(super) pinned: RefCell<Option<gtk::Popover>>,
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
    /// The page and point the page's menu was opened on or Ctrl was clicked at, for Go to Source
    /// and the page commands, until the menu has gone. See [`PdfTab::source_point`] and
    /// [`PdfTab::command_page`].
    pub(super) pointed: Cell<Option<(usize, f32, f32)>>,
    /// The window found this a LaTeX build with no SyncTeX file: the page's menu shows Go to
    /// Source greyed, saying why, rather than not at all.
    pub(super) without_synctex: Cell<bool>,
    /// The line of text Show in PDF asked for while the document was still opening.
    pub(super) spot: Cell<Option<(usize, pdf::Rect)>>,
    /// The write a stroke schedules, which a burst of strokes shares.
    pub(super) autosave: Debounce,
    /// A write of this document is on its way somewhere else — the ssh upload of a remote
    /// vault's copy — and whether another save landed while it was. See [`PdfTab::claim_upload`].
    pub(super) uploading: Cell<bool>,
    pub(super) upload_again: Cell<bool>,
    /// The far end refused the last upload, and the copy beside the document too, and the reader
    /// has been told so once: the strokes after it are refused for the same reason and say
    /// nothing. See [`PdfTab::told_conflict`].
    pub(super) conflict_told: Cell<bool>,
    /// The last upload did not reach the far end at all — a folder it may not write, a full
    /// disk — and the reader has been told once. See [`PdfTab::told_failure`].
    pub(super) failure_told: Cell<bool>,
    /// The last upload did not reach the far end, for whatever reason, so what the file here
    /// holds has yet to go. See [`PdfTab::unsent`].
    pub(super) unsent: Cell<bool>,
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
    /// What was searched for last, under which of the find bar's toggles, so a page edit can run
    /// it again: the matches are filed by page like everything else.
    pub(super) searched: RefCell<(String, Options)>,
    pub(super) matches: RefCell<Vec<(usize, pdf::Rect)>>,
    pub(super) current: Cell<Option<usize>>,
    /// The search running shows its first hit when it lands: one the reader typed, not a page
    /// edit's search again, which leaves the reader where they are.
    pub(super) jump: Cell<bool>,
    pub(super) on_zoom: TabHook,
    pub(super) on_page: TabHook,
    /// Fired just before a jump, so the pane can record where the reader was.
    pub(super) on_jump: TabHook,
    /// Fired when what the Outline pane lists changes: the bookmarks, or a page's comments.
    pub(super) on_outline: TabHook,
    /// Fired when the document's pages are known, which is when it stops being "opening".
    pub(super) on_open: TabHook,
    pub(super) on_matches: TabHook,
    pub(super) on_uri: UriHook,
    pub(super) on_mode: TabHook,
    pub(super) on_history: TabHook,
    pub(super) on_note: NoteHook,
    pub(super) on_export: ExportHook,
    pub(super) on_save_failed: FailHook,
    /// Fired once the file on this machine holds what was drawn, for a vault whose real copy is
    /// somewhere else.
    pub(super) on_saved: TabHook,
    pub(super) on_choice: ChoiceHook,
    pub(super) on_repaged: RepageHook,
    pub(super) on_imported: ImportHook,
    pub relinks: RefCell<Relinks>,
    /// The monitor on a loose file, which nothing else watches (`App::watch_loose`).
    pub monitor: RefCell<Option<gio::FileMonitor>>,
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
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(crate::widgets::FADE_MS)
        .build();
    stack.add_named(&host, Some("view"));
    stack.add_named(&status, Some("status"));
    // In place of the empty pages while a document is slow to open: see [`OPENING`].
    let spinner = adw::Spinner::builder()
        .width_request(32)
        .height_request(32)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    stack.add_named(&spinner, Some("opening"));

    let page = tabs.append(&stack);
    page.set_title(title);
    page.set_tooltip(tooltip);
    page.set_icon(Some(&gio::ThemedIcon::new("x-office-document-symbolic")));

    let thumb_strip = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&thumbs)
        .build();
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
        organize: super::organize::Organize::new(&thumb_strip),
        thumb_strip,
        tx: RefCell::new(None),
        waiting: RefCell::default(),
        inverted: Cell::new(false),
        theme: Cell::new(theme_of(adw::StyleManager::default().is_dark())),
        failed: Cell::new(false),
        resume: Cell::new(None),
        changed: Cell::new(std::time::Instant::now()),
        reload_due: Cell::new(false),
        #[cfg(feature = "bench")]
        opens: Cell::new(0),
        presenting: Cell::new(None),
        pending: Cell::new(Some(place)),
        pending_select: Cell::new(None),
        links: RefCell::new(std::collections::HashMap::new()),
        comments: RefCell::default(),
        tip: RefCell::default(),
        pinned: RefCell::default(),
        glyphs: RefCell::new(std::collections::HashMap::new()),
        selected: RefCell::new(String::new()),
        ranges: RefCell::new(Vec::new()),
        notes: RefCell::new(Vec::new()),
        pending_show: Cell::new(None),
        pointed: Cell::new(None),
        without_synctex: Cell::new(false),
        spot: Cell::new(None),
        autosave: Debounce::new(crate::editor::AUTOSAVE),
        uploading: Cell::new(false),
        upload_again: Cell::new(false),
        conflict_told: Cell::new(false),
        failure_told: Cell::new(false),
        unsent: Cell::new(false),
        saved: Cell::new(None),
        history: Cell::new((false, false)),
        outline: RefCell::new(Vec::new()),
        preview: RefCell::new(None),
        query: Cell::new(0),
        searched: RefCell::default(),
        matches: RefCell::new(Vec::new()),
        current: Cell::new(None),
        jump: Cell::new(false),
        on_zoom: Hook::default(),
        on_page: Hook::default(),
        on_jump: Hook::default(),
        on_outline: Hook::default(),
        on_open: Hook::default(),
        on_matches: Hook::default(),
        on_uri: Hook::default(),
        on_mode: Hook::default(),
        on_history: Hook::default(),
        on_note: Hook::default(),
        on_export: Hook::default(),
        on_save_failed: Hook::default(),
        on_saved: Hook::default(),
        on_choice: Hook::default(),
        on_repaged: Hook::default(),
        on_imported: Hook::default(),
        relinks: RefCell::default(),
        monitor: RefCell::default(),
    });

    tab.view.set_zoom(place.zoom);
    tab.restyle();
    tab.wire(&view);
    // Only the reading view: the strip is fitted to its own width and never changes zoom.
    view.connect_zoom(glib::clone!(
        #[weak]
        tab,
        move || tab.on_zoom.emit(&tab)
    ));
    tab.wire_strip(&thumbs);
    tab.wire_organize();
    tab.wire_drop();
    tab.ring.connect_choice(glib::clone!(
        #[weak]
        tab,
        move |tool, choice| {
            if let Some(f) = tab.on_choice.get() {
                f(&tab, tool, choice);
            }
        }
    ));
    tab.wire_keys();
    tab.wire_preview();
    tab.wire_comments();
    tab.wire_menu();

    // Nothing about the document is known yet, and deliberately so: opening it and measuring its
    // pages is pdfium work, which for a thousand-page file is most of a second, and on a remote
    // vault the bytes are still to be fetched. The tab goes up empty and fills in when the render
    // thread reports back, so the window is on screen in the time it takes to build a widget.
    tab.stack.set_visible_child_name("view");
    glib::timeout_add_local_once(
        OPENING,
        glib::clone!(
            #[weak]
            tab,
            move || {
                let empty = tab.stack.visible_child_name().as_deref() == Some("view");
                if empty && tab.pending.get().is_some() {
                    tab.stack.set_visible_child_name("opening");
                }
            }
        ),
    );
    tab
}

/// How long a document may take to open before a spinner takes the place of its empty pages: as
/// long as a search waits before its progress bar (DESIGN.md, Loading), so one that opens at once
/// never shows one.
const OPENING: std::time::Duration = std::time::Duration::from_millis(160);

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

    /// Start the render thread, which opens the document and then answers requests for it.
    fn start(self: &Rc<Self>) -> Result<(), String> {
        let weak = glib::SendWeakRef::from(self.view.downgrade());
        let tx = render::spawn(self.path(), weak)?;
        for request in self.waiting.take() {
            let _ = tx.send(request);
        }
        *self.tx.borrow_mut() = Some(tx);
        Ok(())
    }

    pub fn page_count(&self) -> usize {
        self.view.page_count()
    }

    /// The page being read: the one under the middle of the reading view.
    pub fn current_page(&self) -> usize {
        self.view.current_page()
    }

    /// The page Add Page Before, Add Page After and Delete Page act on: the one the page's menu
    /// was opened on, else, from the palette or the status bar's page count, the page being read.
    pub fn command_page(&self) -> usize {
        self.pointed
            .take()
            .map_or_else(|| self.current_page(), |(page, ..)| page)
    }

    /// The widget a reader's keys have to reach: the reading view, which is where [`Self::wire_keys`]
    /// puts them. What a window hands the keyboard to when it moves this tab into another pane.
    pub fn key_target(&self) -> gtk::Widget {
        self.view.clone().upcast()
    }

    /// Where the reader is, or, while the pages are not there, where they will be put back.
    pub fn place(&self) -> Place {
        let page = match (self.pending.get(), self.resume.get()) {
            (Some(place), _) => place.page,
            (None, Some(anchor)) => anchor.page,
            (None, None) => self.view.current_page(),
        };
        Place {
            page,
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

    /// The reading view's geometry and the page the strip frames, for `ACCENT_BENCH_PDF` and
    /// nothing else: the headless image has no pointer and no window manager, so the numbers a fit
    /// produced are the only way to see that it fitted.
    #[cfg(feature = "bench")]
    pub fn geometry(&self) -> String {
        format!("{} framed={}", self.view.geometry(), self.thumbs.framed())
    }

    /// What the waiting page says while it is up in place of the pages, and how many times the
    /// render thread has answered an open, either way. Only drills ask.
    #[cfg(feature = "bench")]
    pub fn waiting(&self) -> (Option<String>, u32) {
        let up = self
            .stack
            .visible_child_name()
            .is_some_and(|name| name == "status");
        let said = up.then(|| self.status.description().unwrap_or_default().to_string());
        (said, self.opens.get())
    }

    /// What the reading view and then the strip last painted without. Only drills ask.
    /// The reading view and the thumbnail strip, to tell whether they went with the tab.
    #[cfg(feature = "bench")]
    pub fn views(&self) -> [gtk::Widget; 2] {
        [self.view.clone().upcast(), self.thumbs.clone().upcast()]
    }

    #[cfg(feature = "bench")]
    pub fn unrendered(&self) -> (Vec<String>, Vec<String>) {
        (self.view.unrendered(), self.thumbs.unrendered())
    }

    /// How many tiles the reading view last wanted, how many on screen it last showed other than
    /// sharp, and every tile that has landed since the last ask. Only drills ask.
    #[cfg(feature = "bench")]
    pub fn tiles(&self) -> (usize, usize, Vec<super::TileKey>) {
        let rendered = std::mem::take(&mut self.view.cache().borrow_mut().rendered);
        (self.view.unrendered().len(), self.view.unsharp(), rendered)
    }

    /// Scroll the reading view down by `views` of its own height. Only drills ask.
    #[cfg(feature = "bench")]
    pub fn scroll_by(&self, views: f64) {
        if let Some(v) = gtk::prelude::ScrollableExt::vadjustment(&self.view) {
            v.set_value(v.value() + views * v.page_size());
        }
    }

    /// Scroll the reading view and the strip to these places, in pages from the top: a reader
    /// scrolling one while the other still moves. Only drills ask.
    #[cfg(feature = "bench")]
    pub fn scroll_both(&self, view: f32, strip: f32) {
        for (at, v) in [(view, &self.view), (strip, &self.thumbs)] {
            let page = (at.max(0.0) as usize).min(self.page_count().saturating_sub(1));
            let h = v.page_size(page).map_or(0.0, |(_, h)| h);
            v.goto_page(page, Some(at.fract() * h));
        }
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

    /// The document as it now stands, strokes not yet saved included, with those highlights in
    /// `color`, written to `dest`; the file itself is left as it is. The receiver hears once how
    /// that went, or finds its sender gone when there is no document to copy.
    pub fn copy_to(&self, dest: PathBuf, color: [u8; 3]) -> Receiver<Result<(), String>> {
        let (done, answer) = channel();
        let links = self.notes.borrow().clone();
        self.ask(Request::Copy {
            links,
            color,
            dest,
            done,
        });
        answer
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
        self.on_mode.emit(self);
    }

    /// Whether the tool in hand needs to know what is drawn on the page, which every tool does:
    /// the Adjust tool takes hold of a stroke, the eraser has to find the one under the pointer,
    /// and a stylus's eraser tip erases under any of them.
    pub(super) fn wants_inks(&self) -> bool {
        self.view.mode() != pdfview::Mode::Select
    }

    /// Give those tools every page at least partly on screen.
    pub(super) fn ask_inks(&self) {
        for page in self.view.visible_pages() {
            self.ask(Request::Inks(page));
        }
    }

    /// Ask for the links and comments of every page at least partly on screen, the first time
    /// it is.
    pub(super) fn ask_links(&self) {
        for page in self.view.visible_pages() {
            if !self.links.borrow().contains_key(&page) {
                self.ask(Request::Links(page));
            }
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

    /// Whether the ring is out, for the drills.
    #[cfg(feature = "bench")]
    pub fn ring_visible(&self) -> bool {
        self.ring.widget().is_visible()
    }

    /// What the status bar says while a pen is out, or nothing while one is not. The action's
    /// own label, so the readout, the ring's tooltip and the palette say one word between them.
    pub fn mode_label(&self) -> Option<&'static str> {
        self.view.mode().action().map(crate::actions::label_of)
    }

    /// Take back the last change made in this tab — a stroke drawn, a drag erased, a stroke moved,
    /// a page put in, taken out or moved — through `win.pdf-undo`.
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
        self.on_history.set(Rc::new(f));
    }

    /// Called when the pen is picked up or put down.
    pub fn connect_mode(&self, f: impl Fn(&Rc<PdfTab>) + 'static) {
        self.on_mode.set(Rc::new(f));
    }

    /// A blank page before or `after` the [`PdfTab::command_page`], the size of the page before
    /// it. After the last page it is a notebook's answer to running out of paper.
    pub fn add_page(self: &Rc<Self>, after: bool) {
        let at = self.command_page() + usize::from(after);
        self.edit_pages(pdf::PageEdit::insert(at));
    }

    /// Put a blank page in, take one out, or move one, at once: each is a step Undo takes back, a
    /// page taken out coming back with its ink. Written out on the same timer a stroke is, when
    /// the thread answers with the new pages. The thread refuses to take out the last page — a
    /// PDF keeps one.
    pub fn edit_pages(self: &Rc<Self>, edit: pdf::PageEdit) {
        self.ask(Request::Pages(edit));
    }

    /// Called when a highlight is clicked, with the note holding the link and the byte it is at.
    pub fn connect_note(&self, f: impl Fn(&str, usize) + 'static) {
        self.on_note.set(Rc::new(f));
    }

    /// Called when an export finishes, with what it wrote or why it could not.
    pub fn connect_choice(&self, f: impl Fn(&Rc<PdfTab>, pdfview::Mode, ring::Choice) + 'static) {
        self.on_choice.set(Rc::new(f));
    }

    pub fn connect_export(&self, f: impl Fn(&Rc<PdfTab>, Result<usize, String>) + 'static) {
        self.on_export.set(Rc::new(f));
    }

    /// Called when a drawing could not be written out, with the reason to say.
    pub fn connect_save_failed(&self, f: impl Fn(&Rc<PdfTab>, String) + 'static) {
        self.on_save_failed.set(Rc::new(f));
    }

    /// Called once a write has landed in the file this tab reads, which on a remote vault is the
    /// cached copy and not the document itself.
    pub fn connect_saved(&self, f: impl Fn(&Rc<PdfTab>) + 'static) {
        self.on_saved.set(Rc::new(f));
    }

    /// The bookmarks, for the Outline pane.
    pub fn outline(&self) -> Vec<pdf::Outline> {
        self.outline.borrow().clone()
    }

    /// The bookmark the page being read is under, as its row in [`PdfTab::outline`].
    pub fn bookmark_row(&self) -> Option<usize> {
        covering(&self.outline.borrow(), self.current_page())
    }

    /// The thumbnail strip, for the Outline pane to show under the bookmarks.
    ///
    /// Built once and handed out again on every refresh, so it keeps its scroll position and its
    /// textures; whoever puts it somewhere new takes it out of where it was.
    pub fn thumbnails(&self) -> gtk::Widget {
        self.organize.pane.clone().upcast()
    }

    /// Search the whole document, by case and by whole words where `options` says so, from the
    /// page being read and round, showing the first hit as it lands where `jump` says so. An
    /// empty query clears what is shown.
    pub fn find(self: &Rc<Self>, text: &str, options: Options, jump: bool) {
        // From the hit shown while it is on screen, as the editor searches from its caret: the
        // reveal can leave the middle of the view on the next page, and a query typed further
        // must not move on from the hit it shows. Not after a page edit, which renumbered it.
        let shown = self.current.get().map(|at| self.matches.borrow()[at].0);
        let from = shown
            .filter(|page| jump && self.view.visible_pages().contains(page))
            .unwrap_or_else(|| self.current_page());
        *self.searched.borrow_mut() = (text.to_string(), options);
        self.matches.borrow_mut().clear();
        self.current.set(None);
        self.jump.set(jump);
        self.view.set_marks(std::collections::HashMap::new());
        self.view.set_current_mark(None);
        self.query.set(self.query.get() + 1);
        // Sent even when there is nothing to look for: a query of the same kind takes over from
        // the one running, so this is what stops a search the reader has cleared or closed the
        // bar on, which used to finish the whole document into replies nobody reads.
        self.ask(Request::Search {
            query: self.query.get(),
            text: text.to_string(),
            options,
            from,
            walked: 0,
        });
        self.on_matches.emit(self);
    }

    /// Step to the next or previous match and scroll it into view. With none shown, which a page
    /// edit's search again leaves, the first from the page being read.
    pub fn step_match(self: &Rc<Self>, forward: bool) {
        let total = self.matches.borrow().len();
        if total == 0 {
            return;
        }
        let next = match (self.current.get(), forward) {
            (None, _) => {
                let page = self.current_page();
                let after = self.matches.borrow().partition_point(|(p, _)| *p < page);
                match forward {
                    true => after % total,
                    false => (after + total - 1) % total,
                }
            }
            (Some(at), true) => (at + 1) % total,
            (Some(at), false) => (at + total - 1) % total,
        };
        self.show_match(next);
        self.on_matches.emit(self);
    }

    /// Make match `at` the current one and scroll it into view.
    pub(super) fn show_match(&self, at: usize) {
        self.current.set(Some(at));
        let (page, rect) = self.matches.borrow()[at];
        self.view
            .set_current_mark(Some((page, self.index_on_page(at))));
        self.view.reveal(page, rect);
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

    pub fn connect_zoom(self: &Rc<Self>, f: impl Fn(&Rc<PdfTab>) + 'static) {
        self.on_zoom.set(Rc::new(f));
    }

    pub fn connect_jump(self: &Rc<Self>, f: impl Fn(&Rc<PdfTab>) + 'static) {
        self.on_jump.set(Rc::new(f));
    }

    pub fn connect_page(self: &Rc<Self>, f: impl Fn(&Rc<PdfTab>) + 'static) {
        self.on_page.set(Rc::new(f));
    }

    pub fn connect_outline(self: &Rc<Self>, f: impl Fn(&Rc<PdfTab>) + 'static) {
        self.on_outline.set(Rc::new(f));
    }

    /// Called once the document is open and its pages are known, and again after a reload.
    pub fn connect_opened(self: &Rc<Self>, f: impl Fn(&Rc<PdfTab>) + 'static) {
        self.on_open.set(Rc::new(f));
    }

    /// Whether the render thread is still opening the document.
    pub fn opening(&self) -> bool {
        self.view.page_count() == 0 && !self.failed.get()
    }

    pub fn connect_matches(self: &Rc<Self>, f: impl Fn(&Rc<PdfTab>) + 'static) {
        self.on_matches.set(Rc::new(f));
    }

    pub fn connect_uri(&self, f: impl Fn(&str) + 'static) {
        self.on_uri.set(Rc::new(f));
    }

    /// Called after every page edit, Undo and Redo included, with the edit made and its step:
    /// what the notes that name this document's pages by number have to follow.
    pub fn connect_repaged(&self, f: impl Fn(&Rc<PdfTab>, pdf::PageEdit, u32) + 'static) {
        self.on_repaged.set(Rc::new(f));
    }

    /// Called once another PDF's pages went in, with its name and how many, or why they did not.
    pub fn connect_imported(&self, f: impl Fn(&Rc<PdfTab>, &str, Result<usize, String>) + 'static) {
        self.on_imported.set(Rc::new(f));
    }

    fn show_status(&self, message: &str) {
        let (title, body) = match pdf::available() {
            // The reason is pdfium's and says nothing a reader can act on; what they can do is
            // fix the file, which is what the tab is waiting for.
            true => {
                tracing::debug!("cannot open {}: {message}", self.path().display());
                let name = crate::doc::file_name(&self.key()).to_string();
                (
                    "Cannot Open This PDF",
                    format!("Waiting for {name} to change."),
                )
            }
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
    /// it: say so in place of the pages, wait for the file to change, and tell the window, which
    /// is still showing it as opening or showing its pages.
    ///
    /// Pages that were showing go: they are of a file that is not there any more. Where the
    /// reader was is kept for when it opens again.
    pub(super) fn fail(self: &Rc<Self>, message: &str) {
        if self.view.page_count() > 0 {
            self.resume.set(Some(self.view.anchor()));
            for view in [&self.view, &self.thumbs] {
                view.forget_textures();
                view.set_sizes(Vec::new());
            }
            self.forget_pages();
        }
        if !self.failed.replace(true) && pdf::available() {
            self.watch_while_failed();
        }
        self.show_status(message);
        self.on_open.emit(self);
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
        self.on_jump.emit(self);
    }
}

/// How the pages of a document are coloured under the theme in force.
///
/// Must be called on the main thread: `theme.rs` holds the chosen theme in thread-local state.
pub(super) fn theme_of(dark: bool) -> pdf::Theme {
    match crate::theme::page_colours(dark) {
        Some((paper, ink)) => pdf::Theme::Recolour { paper, ink },
        None => pdf::Theme::Plain,
    }
}

/// The bookmark a reader on `page` is under: of those starting on it or before, the one starting
/// last, and of several starting on the same page the last listed, which is the innermost. `None`
/// above the first bookmark; one naming no page covers nothing.
fn covering(outline: &[pdf::Outline], page: usize) -> Option<usize> {
    outline
        .iter()
        .enumerate()
        .filter_map(|(row, entry)| Some((entry.page?, row)))
        .filter(|(start, _)| *start <= page)
        .max()
        .map(|(_, row)| row)
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

    #[test]
    fn the_bookmark_followed_is_the_one_the_page_is_under() {
        let entry = |depth, page| pdf::Outline {
            depth,
            title: String::new(),
            page,
        };
        // A chapter on the second page with a section on the third, a bookmark naming no page,
        // and an appendix on the fifth.
        let outline = [
            entry(0, Some(1)),
            entry(1, Some(2)),
            entry(0, None),
            entry(0, Some(4)),
        ];
        let rows: Vec<_> = (0..6).map(|page| covering(&outline, page)).collect();
        assert_eq!(rows, [None, Some(0), Some(1), Some(1), Some(3), Some(3)]);
        // A section starting on its chapter's page is the one the page is under.
        let same = [entry(0, Some(2)), entry(1, Some(2))];
        assert_eq!(covering(&same, 2), Some(1));
    }
}
