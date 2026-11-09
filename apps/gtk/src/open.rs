//! Dispatch file opens, read on workers and adopt text, image, diff or status tabs into panes.
//! PDF opening lives in `pdf::window`; Replace All lives in `replace`.

use super::*;
use accent_api::Error;
use accent_core::Error as Core;
use accent_core::note::Syntax;

/// Opening intent determines whether a tab replaces a preview or stays open.
///
/// Browsing tree, search, Git or wikilink results uses `Preview` to avoid accumulating tabs.
/// Open File, drops, renames and command-line opens use `Kept`, the palette `Picked`. Editing,
/// double-clicking or moving a preview into another pane keeps it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Opened {
    Kept,
    Preview,
    /// Kept, and put back by the session restore, which says itself which tab each pane shows
    /// (see [`App::put_back`]): so it lands behind whatever its pane is showing.
    Restored,
    /// Kept, and pinned: a pinned tab dragged in from another window, which is opened here afresh
    /// (`Shell::adopt_page`) and lands at the end of its pane's pinned tabs.
    Pinned,
    /// Keep the nth launch file in requested order, selecting the last regardless of read completion order.
    Launched(usize),
    /// Kept, and split off its pane on that side once it lands: a tree row dropped on a pane's
    /// edge (see [`App::open_beside`]).
    Beside(Side),
    /// Kept, and given the keyboard once in front: picked in the palette, which gives the keyboard
    /// back to where it was opened from (the Files tree, say) before the tab lands.
    Picked,
}

impl App {
    /// Open files from all entry points through one dispatcher.
    ///
    /// Names choose the document kind; text candidates are checked by bytes so binary source
    /// files become status pages rather than unreadable editor buffers.
    pub fn open_path(self: &Rc<Self>, key: &str) {
        self.open_as(key, Opened::Kept);
    }

    /// Open `key` for a look: the tab is this pane's preview, and the next such open takes its
    /// place instead of leaving it behind. What a click in the sidebar and a followed link do.
    pub fn open_preview(self: &Rc<Self>, key: &str) {
        self.open_as(key, Opened::Preview);
    }

    pub fn open_as(self: &Rc<Self>, key: &str, how: Opened) {
        let Some((key, path)) = self.locate(key) else {
            self.pending.fail(key);
            // Deleted since the session named it, or never in this vault: the vault has answered
            // the same way for both, and this machine's disk cannot tell them apart for a remote.
            let shown = doc::host_file_path(key).unwrap_or(key);
            return self.cannot(&format!("open {shown}"), "not in this vault");
        };
        // A note that is already open keeps whatever it is: looking at a real tab again does not
        // demote it, and looking at the preview again does not promote it. One whose read failed
        // is read again, as its Retry does.
        if let Some(doc) = self.doc_for(&key) {
            if let Doc::Loading(held) = &doc {
                self.read_again(held);
                self.reveal_page(&held.page);
                return self.focus_picked(&held.page, how);
            }
            self.drop_awaiting(&key, "not a text file");
            // Open as Text and Open in Accent's Editor asked of it end on that tab too.
            self.pending.take_as_text(&key);
            self.pending.take_in_accent(&key);
            self.reveal_page(doc.page());
            return self.focus_picked(doc.page(), how);
        }
        // So does a file a terminal editor has open, unless what asked needs accent's own buffer.
        if let Some(page) = self.editing(&key) {
            self.reveal_page(&page);
            return self.focus_picked(&page, how);
        }
        let kind = doc::kind_of(&key);
        // Only text becomes a `Tab`, so anything else has nothing for a waiting closure to run on.
        if !matches!(kind, Kind::Note | Kind::Text) {
            self.drop_awaiting(&key, "not a text file");
        }
        match kind {
            Kind::Image => self.open_image(&key, &path, how),
            Kind::Pdf => self.open_pdf(&key, &path, how),
            // Read before their tab can be built, so the tab is held up at once and the
            // document goes into it once it is read.
            Kind::Note | Kind::Diagram | Kind::Notebook | Kind::Text => {
                self.hold(&key, how);
                self.read(&key, &path, how);
            }
        }
    }

    /// Put up the tab `key` will be read into, in its pane and placed as `how` says, before the
    /// read: a click shows its tab at once, however slow the host (DESIGN.md, Loading).
    fn hold(self: &Rc<Self>, key: &str, how: Opened) {
        let tooltip = match doc::host_file_path(key) {
            Some(path) => format!("{path} (remote host)"),
            None => fileops::display_path(&self.root(), key),
        };
        let held = loading::Held::new(key, &self.tabs_for(key), &tooltip);
        self.mark_loose(&held.page, key);
        let page = held.page.clone();
        self.docs.borrow_mut().push(Doc::Loading(held));
        self.select_new_page(&page, how);
        self.place_opened(&page, how);
        self.save_session_soon();
    }

    /// Read `key` into its held tab as the kind its name says.
    pub(crate) fn read(self: &Rc<Self>, key: &str, path: &Path, how: Opened) {
        match doc::kind_of(key) {
            Kind::Note => self.open_text(key, path, Flavour::Note(Syntax::Markdown), how),
            Kind::Diagram => self.open_diagram(key, path, how),
            // Read as text first, whose bytes say whether it reads as a notebook.
            _ => self.open_text(key, path, flavour_of(key), how),
        }
    }

    /// Read a held tab's file again, after its read failed: its Retry, its file opened again, or
    /// the link back.
    pub(crate) fn read_again(self: &Rc<Self>, held: &loading::Held) {
        if !held.failed() {
            return;
        }
        let Some((key, path)) = self.locate(&held.key()) else {
            return self.close_page(&held.page);
        };
        held.again();
        self.read(&key, &path, Opened::Kept);
    }

    /// Whether `key` has a held tab waiting for a read: one closed since, or filled by an earlier
    /// read, has none.
    pub(crate) fn holds(&self, key: &str) -> bool {
        matches!(self.doc_for(key), Some(Doc::Loading(held)) if !held.failed())
    }

    /// Take the held tab of `key` out of the documents for the document read into it, which takes
    /// its place there; `None` once it has been closed.
    pub(crate) fn take_held(&self, key: &str) -> Option<Rc<loading::Held>> {
        let mut docs = self.docs.borrow_mut();
        let at = docs
            .iter()
            .position(|doc| matches!(doc, Doc::Loading(held) if held.key() == key))?;
        match docs.remove(at) {
            Doc::Loading(held) => Some(held),
            _ => None,
        }
    }

    /// A held tab's document is in, in the place its tab was put up in: what waited on it runs,
    /// and the window catches up with it where it is in front. Its pane's selection is left as
    /// the reader left it meanwhile.
    pub(crate) fn landed(self: &Rc<Self>, page: &adw::TabPage, how: Opened) {
        self.reveal_waiting(page);
        // A keep asked for meanwhile of the file rather than of its tab.
        if self
            .doc_for_page(page)
            .is_some_and(|doc| self.pending.take_keep(&doc.key()))
        {
            self.promote(page);
        }
        if self
            .pane_of(page)
            .is_some_and(|pane| pane.tabs.selected_page().as_ref() == Some(page))
        {
            self.sync_active();
        }
        self.focus_picked(page, how);
        self.save_session_soon();
    }

    /// The read of a held tab failed: a file that is not there has no tab, as it never had one,
    /// and any other failure stays in its tab with Retry. What waited on the tab is told.
    pub(crate) fn read_failed(self: &Rc<Self>, key: &str, e: Error) {
        let Some(Doc::Loading(held)) = self.doc_for(key) else {
            return;
        };
        if matches!(e, Error::Core(Core::NotFound(_))) {
            self.close_page(&held.page);
            return self.cannot_open(key, e);
        }
        let why = e.to_string();
        self.drop_awaiting(key, &why);
        self.pending.fail(key);
        let (app, retried) = (Rc::downgrade(self), Rc::downgrade(&held));
        held.fail(&loading::sentence(&why), move || {
            if let (Some(app), Some(held)) = (app.upgrade(), retried.upgrade()) {
                app.read_again(&held);
            }
        });
    }

    /// A new tab with its document in: show what waited for it, and put it where `how` says.
    pub(crate) fn mark_opened(self: &Rc<Self>, page: &adw::TabPage, how: Opened) {
        self.reveal_waiting(page);
        self.place_opened(page, how);
    }

    /// Show in `page`'s document what waited for it to open.
    fn reveal_waiting(self: &Rc<Self>, page: &adw::TabPage) {
        // A label or a page a diagram waits to show, or a notebook's search hit, which a file
        // that turned out to be text drops.
        if let Some(doc) = self.doc_for_page(page) {
            let reveal = self.pending.take_reveal(&doc.key());
            match (reveal, &doc) {
                (Some(Reveal::Diagram(reveal)), Doc::Diagram(tab)) => reveal(self, tab),
                (Some(Reveal::NotebookCell(cell, line, column)), Doc::Notebook(tab)) => {
                    tab.goto_place(cell, line, column);
                }
                (Some(Reveal::NotebookHit(bytes)), Doc::Notebook(tab)) => tab.reveal_hit(bytes),
                (Some(Reveal::NotebookHit(bytes)), Doc::Text(tab)) => {
                    select_text_range(tab, bytes);
                }
                (Some(Reveal::NotebookFilePos(at)), Doc::Notebook(tab)) => {
                    tab.go_to_file(at);
                }
                (Some(Reveal::NotebookFilePos(at)), Doc::Text(tab)) => {
                    if doc::is_loose_key(&tab.rel())
                        && let Some(vault) = self.language_vault(tab)
                    {
                        lang::join(tab, vault);
                    }
                    tab.goto_pos(at);
                }
                // A PDF's pages and the notes' highlights on them come after its tab, so a
                // print or an export waits for them (`App::sync_pdf_links`).
                (Some(Reveal::Then(run)), Doc::Pdf(pdf)) if pdf.opening() => {
                    self.pending.reveal(&doc.key(), Reveal::Then(run));
                }
                (Some(Reveal::Then(run)), doc) => run(self, doc),
                // Meant for a terminal editor and landed in accent's own: at the same place.
                (Some(Reveal::Line(line, column)), Doc::Text(tab)) => {
                    let column = column.unwrap_or(1);
                    tab.goto_pos(accent_api::Pos {
                        line: line.saturating_sub(1),
                        character: column.saturating_sub(1),
                    });
                }
                _ => {}
            }
        }
    }

    /// Put a new `page` where `how` says: pinned, in the command line's order, split off, or as
    /// its pane's preview, replacing the previous one and applying keep state.
    ///
    /// Insert the new tab before closing the old one so the pane cannot close while briefly
    /// empty. Earlier tree double-clicks keep the arriving tab; launch tabs retain named order.
    fn place_opened(self: &Rc<Self>, page: &adw::TabPage, how: Opened) {
        // A double click in the Files tree whose second press came before the tab did.
        let keep = self
            .doc_for_page(page)
            .is_some_and(|doc| self.pending.take_keep(&doc.key()));
        if how == Opened::Pinned {
            return self.set_pinned(page, true);
        }
        if let Opened::Launched(at) = how {
            return self.land_in_order(page, at);
        }
        // What its pane showed before it landed stays in front there.
        if let (Opened::Beside(side), Some(pane)) = (how, self.pane_of(page)) {
            self.select_survivor(page);
            return self.split_page(&pane, side, page);
        }
        self.focus_picked(page, how);
        if how != Opened::Preview {
            return;
        }
        let Some(pane) = self.pane_of(page) else {
            return;
        };
        // The preview it replaces goes while it is still one, which a terminal editor's question
        // is not asked of.
        if let Some(old) = pane.preview().filter(|old| old != page) {
            pane.tabs.close_page(&old);
        }
        pane.set_preview(page);
        if keep {
            pane.keep(page);
        }
    }

    /// Insert each arriving launch tab before later-named tabs already present; select the last requested file.
    /// PDFs and worker-read text arrive at different times, so completion order cannot define bar order.
    fn land_in_order(&self, page: &adw::TabPage, at: usize) {
        self.launched.borrow_mut().push((at, page.downgrade()));
        let Some(pane) = self.pane_of(page) else {
            return;
        };
        let landed: Vec<(usize, adw::TabPage)> = self
            .launched
            .borrow()
            .iter()
            .filter_map(|(named, landed)| Some((*named, landed.upgrade()?)))
            .filter(|(_, landed)| pane.has(landed))
            .collect();
        let later = landed.iter().filter(|(named, _)| *named > at);
        if let Some((_, later)) = later.min_by_key(|(named, _)| *named) {
            pane.tabs.reorder_page(page, pane.tabs.page_position(later));
        }
        if let Some((_, last)) = landed.iter().max_by_key(|(named, _)| *named) {
            pane.tabs.set_selected_page(last);
        }
    }

    /// The preferences every text tab is built with.
    fn prefs(&self) -> Prefs {
        let config = self.config.borrow();
        Prefs {
            spellcheck: config.spellcheck,
            ghost_text: config.ghost_text,
            font: config.editor_font.clone(),
            zoom: self.zoom.get(),
            column_width: config.column_width,
            indent_width: config.indent_width,
            minimap: config.minimap,
            line_numbers: config.line_numbers,
        }
    }

    /// Read text on a worker before opening an editor, or choose another tab kind from the bytes.
    /// Remote round trips must not block the window.
    fn open_text(self: &Rc<Self>, key: &str, path: &Path, flavour: Flavour, how: Opened) {
        // Host dependencies read through the host's named-file gate; other loose files are local.
        let vault = self
            .vault()
            .filter(|_| !doc::is_loose_key(key) || doc::host_file_path(key).is_some())
            .cloned();
        let (key, path) = (key.to_string(), path.to_path_buf());
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let read = crate::work::off_thread("reader", {
                let key = key.clone();
                move || {
                    // A slow host, for `ACCENT_BENCH_TABS=loading` and `tree:`.
                    #[cfg(feature = "bench")]
                    crate::bench::hold(&crate::bench::SLOW_READ_MS);
                    match vault {
                        Some(vault) if doc::host_file_path(&key).is_some() => {
                            vault.read_named(doc::host_file_path(&key).unwrap())
                        }
                        Some(vault) => vault.read_text(&key),
                        None => accent_core::fs::read_text(&path).map_err(Into::into),
                    }
                }
            })
            .await;
            // Its tab closed while it was on its way, or an earlier read of the file, whose tab
            // was closed, filled the tab this one was for: nothing is built and nothing written.
            let Some(app) = weak.upgrade().filter(|app| app.holds(&key)) else {
                return;
            };
            // A worker that panicked is one more way for the read to fail, and is said as the others
            // are: the open named in a toast, and whatever was waiting on the tab let go with it.
            let read = read.unwrap_or_else(|| Err(Core::Io("the worker stopped".into()).into()));
            app.adopt_text(&key, read, flavour, how);
        });
    }

    /// What [`open_text`](Self::open_text) does once the bytes are in hand.
    fn adopt_text(
        self: &Rc<Self>,
        key: &str,
        read: accent_api::Result<accent_core::fs::Read>,
        flavour: Flavour,
        how: Opened,
    ) {
        // Asked of this open, so its read takes them whatever the file turns out to be.
        let as_text = self.pending.take_as_text(key);
        let in_accent = self.pending.take_in_accent(key);
        let text = match read {
            Ok(accent_core::fs::Read::Text(text)) => text,
            Ok(accent_core::fs::Read::Binary { size }) => {
                return self.open_status(
                    key,
                    "Binary File",
                    &format!("This file cannot be edited as text ({}).", human(size)),
                    how,
                );
            }
            Ok(accent_core::fs::Read::TooLarge { size }) => {
                // Too large for accent's editor, not for a terminal editor.
                if matches!(doc::kind_of(key), Kind::Note | Kind::Text)
                    && self.open_in_editor(key, how, in_accent)
                {
                    return;
                }
                return self.open_status(
                    key,
                    "File Too Large",
                    // The file's size goes through `human`, which is decimal because that is what
                    // GNOME shows in Files. The cap does not: it is `16 * 1024 * 1024`, and
                    // decimal units render that as "16.8 MB", which is not the number anyone set.
                    &format!(
                        "{} exceeds the {} MiB editor limit.",
                        human(size),
                        accent_core::fs::MAX_TEXT / (1024 * 1024)
                    ),
                    how,
                );
            }
            Err(e) => return self.read_failed(key, e),
        };
        // An `.xml` that draw.io wrote is a diagram, whatever its name says: known by its bytes.
        // A search hit in it waits for the diagram instead of the text tab it asked for.
        if doc::host_file_path(key).is_none() && accent_core::path::holds_diagram(key, &text.text) {
            if let Some(what) = self.pending.retarget(key) {
                self.cannot(&what, "a diagram, not a text file");
            }
            return self.open_diagram_text(key, text, how);
        }
        // So is an `.ipynb`, and a `.py` marimo wrote, unless the reader asked for its text; a
        // search hit in it waits for the notebook as one in a diagram does.
        if doc::host_file_path(key).is_none()
            && !as_text
            && accent_core::path::holds_notebook(key, &text.text)
        {
            if let Some(what) = self.pending.retarget(key) {
                self.cannot(&what, "a notebook, not a text file");
            }
            return self.open_notebook_text(key, text, flavour, how);
        }
        // In a terminal editor's tab instead, which takes the held one's place.
        if !as_text && self.open_in_editor(key, how, in_accent) {
            if let Some(held) = self.take_held(key) {
                self.close_page(&held.page);
            }
            return;
        }
        self.adopt_text_tab(key, text, flavour, how);
    }

    /// A text tab of `text`, read into its held tab and wired into the window; `None` once that
    /// tab has been closed.
    pub(crate) fn adopt_text_tab(
        self: &Rc<Self>,
        key: &str,
        text: accent_core::fs::Text,
        flavour: Flavour,
        how: Opened,
    ) -> Option<Rc<Tab>> {
        let held = self.take_held(key)?;
        let prefs = self.prefs();
        let tab = editor::open(&self.root(), key, text, flavour, &held, &prefs);
        self.watch_bibliography(&tab);
        self.adopt(tab.clone(), how);
        self.sync_conflict_banner(key, None);
        Some(tab)
    }

    /// Insert Template…: a template's text at the caret of the active note, whose stem is what
    /// the template's `{{title}}` means.
    pub fn insert_template(self: &Rc<Self>) {
        let Some(tab) = self.active() else {
            return self.cannot("insert a template", "no note is open");
        };
        let Some(ops) = self.need_ops("insert a template") else {
            return;
        };
        let title = Path::new(&tab.rel())
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Weak: the template is rendered on a worker, and the tab may close before it lands.
        let tab = Rc::downgrade(&tab);
        fileops::insert_template(
            ops,
            &title,
            Box::new(move |text, stops| {
                if let Some(tab) = tab.upgrade() {
                    tab.insert_stops(text, stops);
                }
            }),
        );
    }

    /// An image, in a tab that only looks at it.
    fn open_image(self: &Rc<Self>, key: &str, path: &Path, how: Opened) {
        let picture = gtk::Picture::new();
        picture.set_content_fit(gtk::ContentFit::ScaleDown);
        picture.set_can_shrink(true);
        let scroller = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&picture)
            .build();
        crate::widgets::smooth_wheel(&scroller);
        // Held back until it is decoded, the loading page up meanwhile once the open has taken a
        // moment: on a remote vault its bytes come first.
        let holder = loading::holder();
        loading::put(&holder, &scroller);
        let image = self.adopt_viewer(Doc::Image, key, &holder, doc::icon_for(key), how);
        *image.monitor.borrow_mut() = self.watch_loose(key, path);
        // An SVG is drawn at the display's scale, so a window moved onto another display draws it
        // again (`show_image` asks for the scale with the look).
        let shown = Rc::downgrade(&image);
        picture.connect_scale_factor_notify(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_| {
                if let Some(image) = shown.upgrade() {
                    app.show_image(&image, None);
                }
            }
        ));
        // On the scroller rather than the picture: while the image is fitted it is smaller than
        // the viewport, and a wheel over the empty space around it has to zoom too. Bubble
        // phase, ahead of the scroller's own controller, as everywhere else.
        let (viewer, pointer) = (Rc::downgrade(&image), track_pointer(&scroller));
        zoom_on_wheel(
            &scroller,
            gtk::PropagationPhase::Bubble,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |out| {
                    if let Some(image) = viewer.upgrade() {
                        app.zoom_image(&image, Some(out), pointer.get());
                    }
                }
            ),
        );
        // Two fingers zoom it in the wheel's tenths, around the point between them, as a PDF
        // page does; on the scroller for the same reason.
        let (viewer, pinched) = (Rc::downgrade(&image), Rc::downgrade(&image));
        zoom_on_pinch(
            &scroller,
            move || {
                pinched
                    .upgrade()
                    .and_then(|image| zoom::image_zoom(&image))
                    .unwrap_or(1.0)
            },
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |zoom, at| {
                    if let Some(image) = viewer.upgrade()
                        && image.zoom.get() != Some(zoom)
                    {
                        app.zoom_image_to(&image, Some(zoom), Some(at));
                    }
                }
            ),
        );
        // The decoder needs real bytes, and the protocol deliberately carries none.
        let viewer = Rc::downgrade(&image);
        self.local_copy(key, path, move |app, copy| {
            let Some(image) = viewer.upgrade() else {
                return;
            };
            match copy {
                Ok(copy) => app.show_image(&image, Some(copy)),
                Err(e) => app.image_failed(&image, e),
            }
        });
    }

    /// An image's bytes did not come. One that never showed says so in its tab, its Retry
    /// fetching it again as a change on the host does; one on screen keeps what it shows.
    pub(crate) fn image_failed(self: &Rc<Self>, image: &Rc<doc::Viewer>, e: Error) {
        let Some(holder) = loading::holder_of(&image.page).filter(|_| image.size.get().is_none())
        else {
            return self.cannot("reload", e);
        };
        let (key, retry) = (image.key(), Rc::downgrade(self));
        let again = key.clone();
        self.fetch_failed(&holder, &key, &image.page, e, move || {
            if let Some(app) = retry.upgrade() {
                app.changed_on_disk(&again);
            }
        });
    }

    /// A viewer's bytes did not come: a file that is not there closes its tab, as a note that is
    /// not there never has one, and any other failure stays in the tab, in `stack`, with Retry.
    pub(crate) fn fetch_failed(
        self: &Rc<Self>,
        stack: &gtk::Stack,
        key: &str,
        page: &adw::TabPage,
        e: Error,
        retry: impl Fn() + 'static,
    ) {
        if matches!(e, Error::Core(Core::NotFound(_))) {
            return self.gone(key.to_string(), page, e);
        }
        let why = loading::sentence(&e.to_string());
        let stacked = stack.downgrade();
        loading::fail(stack, doc::file_name(key), &why, move || {
            if let Some(stack) = stacked.upgrade() {
                loading::again(&stack);
            }
            retry();
        });
        // A PDF's status bar said it was opening.
        self.sync_opening();
    }

    /// Decode or recolour an image off-thread using `read` or its cached texture.
    ///
    /// Skip unchanged appearances and apply only the latest result. If appearance changed while
    /// decoding, request another render so theme changes and inversions are retained.
    pub(crate) fn show_image(self: &Rc<Self>, image: &Rc<doc::Viewer>, read: Option<PathBuf>) {
        let reading = read.is_some();
        let (path, original) = match read {
            // What is on screen is of the old contents, so a restyle meanwhile waits for these.
            Some(path) => {
                image.image.take();
                (path, None)
            }
            None => match image.image.borrow().clone() {
                Some(read) => read,
                // Still being read, under a look its landing will find is not this one.
                None => return,
            },
        };
        let scale = picture_of(&image.page).map_or(1, |p| p.scale_factor());
        let wanted = (
            look::Look::now(),
            self.inverted_images.borrow().contains(&image.key()),
            scale,
            look::drawn_zoom(&path, image.zoom.get(), image.size.get(), scale),
        );
        if !reading && image.look.get() == Some(wanted) {
            return;
        }
        image.look.set(Some(wanted));
        let ticket = image.shows.get().wrapping_add(1);
        image.shows.set(ticket);
        let (app, image) = (Rc::downgrade(self), Rc::downgrade(image));
        glib::spawn_future_local(async move {
            let ((look, inverted, scale, zoom), from) = (wanted, path.clone());
            let shown = work::off_thread("image", move || {
                look::show(&from, original, look, inverted, scale, zoom, None)
            })
            .await;
            let (Some(app), Some(image)) = (app.upgrade(), image.upgrade()) else {
                return;
            };
            let Some(picture) = picture_of(&image.page).filter(|_| image.shows.get() == ticket)
            else {
                return;
            };
            match shown {
                Some(Ok(shown)) => {
                    picture.set_paintable(Some(&shown.paintable()));
                    // In place of the loading page, the first time.
                    if let Some(holder) = loading::holder_of(&image.page) {
                        loading::show(&holder);
                    }
                    // The size a zoomed picture asks for is worked out from its paintable, so a
                    // file of another size would be drawn at its own while the readout kept the
                    // old percentage. Asked again of this one, so the zoom means the same thing
                    // either side of a reload.
                    zoom::set_image_zoom(&picture, image.zoom.get());
                    let own = (shown.texture.width(), shown.texture.height());
                    image.size.set(Some(shown.size.unwrap_or(own)));
                    app.sync_details();
                    app.sync_image_invert();
                    *image.image.borrow_mut() = Some((path, shown.original));
                    app.show_image(&image, None);
                }
                // A file that never showed says why in its tab, as one that never arrived does;
                // one that did keeps its last contents up.
                Some(Err(e)) if picture.paintable().is_none() => {
                    app.image_failed(&image, Core::Invalid(e.to_string()).into())
                }
                Some(Err(e)) => app.cannot("reload", e),
                None => app.cannot("reload", "the image worker stopped"),
            }
        });
    }

    /// Offer Invert Image Colours over an image alone.
    pub(crate) fn sync_image_invert(&self) {
        let image = matches!(self.active_doc(), Some(Doc::Image(_)));
        let action = self.window.lookup_action("image-invert");
        if let Some(action) = action.and_downcast::<gio::SimpleAction>() {
            action.set_enabled(image);
        }
    }

    /// Toggle key's image appearance in tabs and preview for this process, reversing theme recolouring.
    pub(crate) fn invert_image(self: &Rc<Self>, key: &str) {
        {
            let mut inverted = self.inverted_images.borrow_mut();
            if !inverted.remove(key) {
                inverted.insert(key.to_string());
            }
        }
        for image in self.images().iter().filter(|image| image.key() == key) {
            self.show_image(image, None);
        }
        self.reshow_preview_image(key);
    }

    /// Resolve `key` to local bytes for PDF and image readers, then call `landed`.
    ///
    /// Local and loose files need no transfer; remote files download on a worker with status
    /// progress. Current cached copies are silent. PDF opening waits until the download finishes.
    pub(crate) fn local_copy(
        self: &Rc<Self>,
        key: &str,
        path: &Path,
        landed: impl FnOnce(&Rc<App>, accent_api::Result<PathBuf>) + 'static,
    ) {
        let vault = self.vault().filter(|_| !doc::is_loose_key(key)).cloned();
        let remote = vault.as_ref().is_some_and(|v| v.is_remote());
        let (key, path) = (key.to_string(), path.to_path_buf());
        if remote {
            self.fetching.borrow_mut().insert(key.clone());
            self.sync_opening();
        }
        let bytes = Arc::new(statusbar::Bytes::default());
        let what = format!("Downloading {}", accent_core::path::basename(&key));
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let work = crate::work::off_thread("fetch", {
                let (key, bytes) = (key.clone(), bytes.clone());
                move || {
                    #[cfg(feature = "bench")]
                    crate::bench::hold(&crate::bench::SLOW_READ_MS);
                    match vault {
                        Some(vault) => {
                            vault.fetch_with(&key, &|done, total| bytes.set(done, total))
                        }
                        None => Ok(path),
                    }
                }
            });
            let (say, retell) = (weak.clone(), weak.clone());
            let copy = statusbar::counting(
                move |text, running| {
                    if let Some(app) = say.upgrade() {
                        app.statusbar.set_transfer(text, running);
                    }
                },
                move |from, to| {
                    if let Some(app) = retell.upgrade() {
                        app.statusbar.retell_transfer(from, to);
                    }
                },
                move || bytes.line(&what),
                work,
            )
            .await
            .unwrap_or_else(|| Err(Core::Io("the fetch stopped".into()).into()));
            if let Some(app) = weak.upgrade() {
                if remote {
                    app.fetching.borrow_mut().remove(&key);
                    app.sync_opening();
                }
                landed(&app, copy);
            }
        });
    }

    /// A viewer whose bytes never came: its tab goes, the way a note that would not open never
    /// gets one, and the reason is said.
    pub(crate) fn gone(&self, key: String, page: &adw::TabPage, e: Error) {
        self.close_page(page);
        self.cannot_open(&key, e);
    }

    /// A file we decline to open, as a page saying why (DESIGN.md, States: a status page with one
    /// sentence and at most one button), in its held tab.
    pub(crate) fn open_status(self: &Rc<Self>, key: &str, title: &str, body: &str, how: Opened) {
        self.drop_awaiting(key, &title.to_lowercase());
        let Some(held) = self.take_held(key) else {
            return;
        };
        let key = key.to_string();
        let status = adw::StatusPage::builder()
            .icon_name("dialog-warning-symbolic")
            .title(title)
            .description(body)
            .build();
        // The vault downloader takes vault paths; a host dependency has no such path.
        if doc::host_file_path(&key).is_none() {
            let on_host = self.on_host(&key);
            let button = gtk::Button::builder()
                .label(if on_host {
                    "Download…"
                } else {
                    "Show in Files"
                })
                .halign(gtk::Align::Center)
                .css_classes(["pill"])
                .build();
            button.connect_clicked(glib::clone!(
                #[weak(rename_to = app)]
                self,
                #[strong]
                key,
                move |_| {
                    if !on_host {
                        let path = app.root().join(&key);
                        let toast = app.clone();
                        fileops::reveal(&app.window, &path, move |m| toast.toast(m));
                    } else if let Some(ops) = app.ops() {
                        fileops::download(ops, &key);
                    }
                }
            ));
            status.set_child(Some(&button));
        }
        let page = held.fill(&status);
        page.set_icon(Some(&gio::ThemedIcon::new("dialog-warning-symbolic")));
        self.watch_folder_of(&key);
        self.docs
            .borrow_mut()
            .push(Doc::Status(doc::Viewer::new(&key, page.clone())));
        self.landed(&page, how);
    }

    /// Open or refresh a blob comparison keyed by `key`, using `file` for syntax styling.
    ///
    /// Git-row comparisons use preview tabs to avoid accumulating one per click. Restored
    /// comparisons remain kept, behind the session's selected tab in their restored pane.
    pub fn open_diff(
        self: &Rc<Self>,
        key: &str,
        file: &str,
        title: &str,
        old: (&str, &str),
        new: (&str, &str),
        restored: bool,
    ) -> Rc<difftab::DiffTab> {
        if let Some(Doc::Diff(tab)) = self.doc_for(key) {
            tab.set_texts(old.1, new.1);
            if !restored {
                self.reveal_page(&tab.page);
            }
            return tab;
        }
        let flavour = match doc::kind_of(file) {
            Kind::Note => Flavour::Note(Syntax::Markdown),
            _ => flavour_of(file),
        };
        let font = self.config.borrow().editor_font.clone();
        let tab = difftab::DiffTab::open(
            &self.tabs_for(key),
            key,
            file,
            title,
            flavour,
            old,
            new,
            font.as_deref(),
            self.zoom.get(),
        );
        tab.set_indent_width(self.config.borrow().indent_width);
        self.docs.borrow_mut().push(Doc::Diff(tab.clone()));
        let how = match restored {
            true => Opened::Restored,
            false => Opened::Preview,
        };
        self.select_new_page(&tab.page, how);
        self.mark_opened(&tab.page, how);
        tab
    }

    /// Put a tab with no buffer into the window: [`App::open_image`]'s.
    fn adopt_viewer(
        self: &Rc<Self>,
        wrap: fn(Rc<doc::Viewer>) -> Doc,
        key: &str,
        child: &impl IsA<gtk::Widget>,
        icon: &str,
        how: Opened,
    ) -> Rc<doc::Viewer> {
        let page = self.tabs_for(key).append(child);
        page.set_title(doc::file_name(key));
        let tooltip = match doc::host_file_path(key) {
            Some(path) => format!("{path} (remote host)"),
            None => fileops::display_path(&self.root(), key),
        };
        page.set_tooltip(&tooltip);
        page.set_icon(Some(&gio::ThemedIcon::new(icon)));
        self.mark_loose(&page, key);
        let viewer = doc::Viewer::new(key, page.clone());
        self.watch_folder_of(key);
        self.docs.borrow_mut().push(wrap(viewer.clone()));
        self.select_new_page(&page, how);
        self.mark_opened(&page, how);
        self.save_session_soon();
        viewer
    }

    /// Open a sidebar search/tag hit as a preview and reveal its range after loading.
    /// Successive browsing hits reuse the preview tab.
    pub fn open_note_at(self: &Rc<Self>, rel: &str, at: Option<sidebar::Target>) {
        self.mark();
        match at {
            // In the terminal editor picked, at the hit's line, where the file opens in one.
            Some(sidebar::Target::Range(_, Some(line)))
                if self.edit_at(rel, (line, None), Opened::Preview) => {}
            Some(sidebar::Target::Range(bytes, _)) if doc::kind_of(rel) == Kind::Diagram => {
                self.open_label(rel, bytes.start)
            }
            // Only its bytes say whether an `.xml` is a diagram: the hit lands on the label if it
            // is one and on the text if it is not, whichever tab it opens as.
            Some(sidebar::Target::Range(bytes, _)) if accent_core::path::may_be_diagram(rel) => {
                match self.doc_for(rel) {
                    Some(Doc::Diagram(_)) => self.open_label(rel, bytes.start),
                    Some(_) => self.select_when_open(rel, sidebar::Target::Range(bytes, None)),
                    None => {
                        let at = bytes.start;
                        let reveal = Reveal::Diagram(Box::new(move |_, tab| reveal_label(tab, at)));
                        self.pending.reveal(rel, reveal);
                        self.select_when_open(rel, sidebar::Target::Range(bytes, None));
                    }
                }
            }
            Some(sidebar::Target::Range(bytes, _)) if accent_core::path::may_be_notebook(rel) => {
                self.open_cell(rel, bytes)
            }
            Some(sidebar::Target::Page { page, text }) => {
                self.open_preview(rel);
                if let Some(Doc::Pdf(pdf)) = self.doc_for(rel) {
                    pdf.show_hit(page, &text);
                }
            }
            Some(at) => self.select_when_open(rel, at),
            None => self.open_preview(rel),
        }
    }

    /// Open diagram `rel` at the cell matching byte offset `at` in indexed label lines.
    /// Reveal its page and cell once a pending worker read lands ([`mark_opened`](Self::mark_opened)).
    fn open_label(self: &Rc<Self>, rel: &str, at: usize) {
        self.reveal_in_diagram(rel, move |_, tab| reveal_label(tab, at));
    }

    /// Open notebook `rel` on the cell holding `bytes` of what the index holds of it, the match
    /// selected; a `.py` that turns out not to be a notebook opens as text, on the match.
    fn open_cell(self: &Rc<Self>, rel: &str, bytes: std::ops::Range<usize>) {
        match self.doc_for(rel) {
            Some(Doc::Notebook(tab)) => {
                self.reveal_page(&tab.page);
                tab.reveal_hit(bytes);
            }
            Some(_) => self.select_when_open(rel, sidebar::Target::Range(bytes, None)),
            None => {
                self.pending.reveal(rel, Reveal::NotebookHit(bytes.clone()));
                // Only text has a tab to wait on, and only its bytes say what a `.py` is.
                match accent_core::path::is_notebook(rel) {
                    true => self.open_preview(rel),
                    false => self.select_when_open(rel, sidebar::Target::Range(bytes, None)),
                }
            }
        }
    }

    /// Open diagram `rel` on its page called `name`, as a link to it lands: as a jump, or with a
    /// toast saying there is none.
    fn open_diagram_page(self: &Rc<Self>, rel: &str, name: &str) {
        let name = name.to_string();
        let show = move |app: &Rc<App>, tab: &Rc<crate::diagram::DiagramTab>| {
            match crate::diagram::page_named(&tab.file(), &name) {
                Some(i) => tab.goto_page(i),
                None => app.toast(&format!("No page {name} in {}", doc::file_name(&tab.key()))),
            }
        };
        self.reveal_in_diagram(rel, show);
    }

    /// Show `reveal` in diagram `rel`, now if it is open, else once a worker has read it
    /// ([`mark_opened`](Self::mark_opened)).
    fn reveal_in_diagram(
        self: &Rc<Self>,
        rel: &str,
        reveal: impl FnOnce(&Rc<App>, &Rc<crate::diagram::DiagramTab>) + 'static,
    ) {
        if let Some(Doc::Diagram(tab)) = self.doc_for(rel) {
            self.reveal_page(&tab.page);
            return reveal(self, &tab);
        }
        self.pending.reveal(rel, Reveal::Diagram(Box::new(reveal)));
        self.open_preview(rel);
    }

    /// Put the caret over `at` once `rel` has a tab, whoever opened it: a search hit, a tag or a
    /// followed link. A tag names itself rather than a place, because only the note knows where
    /// it writes it.
    fn select_when_open(self: &Rc<Self>, rel: &str, at: sidebar::Target) {
        self.with_tab(rel, Opened::Preview, "open", move |_, tab| match &at {
            sidebar::Target::Range(bytes, _) => select_text_range(tab, bytes.clone()),
            sidebar::Target::Tag(name) => tab.goto_tag(name),
            // A PDF's, which has no text tab: `open_note_at` takes it.
            sidebar::Target::Page { .. } => {}
        });
    }

    /// Resolve written wikilink targets by shortest matching path; empty names target the current note.
    /// Missing files offer New File with the requested path, allowing links to create notes.
    pub fn open_target(self: &Rc<Self>, target: &str) {
        let Some(vault) = self.vault() else {
            return self.needs_vault("follow a link");
        };
        // `paper.pdf#page=3&selection=…` and `Note#Heading` resolve by the path and land by the
        // anchor: a PDF's page, or a note's heading.
        let (target, anchor) = target.split_once('#').unwrap_or((target, ""));
        // Where the link was, so Back returns to it. Before the open, and before the selection
        // change it causes records the same place, which coalesces into this one.
        self.mark();
        let anchor = anchor.to_string();
        if target.is_empty() {
            // A preview tab's note is the current one there.
            let rel = match self.active_doc() {
                Some(Doc::Rendered(rendered)) => rendered.note().map(|note| note.rel()),
                doc => doc.map(|doc| doc.key()),
            };
            if let Some(rel) = rel {
                self.land_on(&rel, &anchor);
            }
            return;
        }
        // Resolved on a worker: the index that knows the name is on the host for a remote vault.
        let (vault, target) = (vault.clone(), target.to_string());
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let asked = target.clone();
            let resolved =
                crate::work::attempt(&format!("resolve {asked}"), move || vault.follow(&asked))
                    .await;
            let Some(app) = weak.upgrade() else { return };
            match resolved {
                Ok(Some(rel)) => app.land_on(&rel, &anchor),
                // Nothing answers to the name: offer to write it, prefilled with what the link
                // says. Cancelling says nothing — the reader followed a link and changed
                // their mind about creating the note behind it.
                Ok(None) => {
                    if let Some(ops) = app.ops() {
                        fileops::new_linked_note(ops, &target);
                    }
                }
                Err(why) => app.toast(&why),
            }
        });
    }

    /// Open `rel` and land where `anchor` says: a PDF's page and selection, a diagram's page, or
    /// a note's heading.
    fn land_on(self: &Rc<Self>, rel: &str, anchor: &str) {
        match accent_core::markdown::pdf_anchor(anchor) {
            Some(at) => {
                self.open_preview(rel);
                self.show_pdf_anchor(rel, Some(at));
            }
            None if anchor.is_empty() => self.open_preview(rel),
            None if doc::kind_of(rel) == Kind::Diagram => self.open_diagram_page(rel, anchor),
            None => {
                let anchor = anchor.to_string();
                self.with_tab(rel, Opened::Preview, "open", move |app, tab| {
                    if !tab.goto_heading(&anchor) {
                        app.no_heading(tab, &anchor);
                    }
                });
            }
        }
    }

    /// Report missing heading/block anchors and place the caret at the top for definition and preview jumps.
    pub fn no_heading(&self, tab: &Tab, anchor: &str) {
        tab.goto_pos(accent_api::Pos::default());
        let name = doc::file_name(&tab.rel()).to_string();
        let what = accent_core::note::looked_for(&tab.rel(), anchor);
        self.toast(&format!("No {what} in {name}"));
    }

    /// Apply a PDF anchor immediately after opening its tab.
    /// on_tab waits for text editors, while open_pdf registers its document before returning.
    pub fn show_pdf_anchor(&self, key: &str, anchor: Option<PdfAnchor>) {
        let Some((page, selection)) = anchor else {
            return;
        };
        if let Some(Doc::Pdf(pdf)) = self.doc_for(key) {
            pdf.show_link(page, selection);
        }
    }

    /// Resolve a vault key, local loose path, or remote host-file key to its read path.
    ///
    /// Reject escaping wikilinks here, since note content reaches this entry point directly.
    /// Avoid canonicalisation so intentionally indexed directory symlinks still work. Existence
    /// checks stay on the reader's worker to avoid a remote round trip on the main thread;
    /// [`cannot_open`](Self::cannot_open) reports failure.
    pub(crate) fn locate(&self, key: &str) -> Option<(String, PathBuf)> {
        if key.starts_with("host-file:") {
            let path = doc::host_file_path(key)?;
            if !matches!(doc::kind_of(key), Kind::Note | Kind::Text) {
                return None;
            }
            return self
                .vault()
                .is_some_and(|vault| vault.is_remote())
                .then(|| (key.to_string(), PathBuf::from(path)));
        }
        if doc::is_loose_key(key) {
            let path = PathBuf::from(key);
            return path.is_file().then(|| (key.to_string(), path));
        }
        // A window with no vault has nothing to be relative to, so only absolute keys open.
        let path = self.vault()?.resolve(key).ok()?;
        // Normalised, so `./a.md` and `a.md` are one tab rather than two.
        let key = path.strip_prefix(self.root()).ok()?.to_str()?.to_string();
        Some((key, path))
    }

    /// A file that would not open. One that is not there reads the same whoever found out: the
    /// session naming a note deleted since, or a link to one never written.
    pub(crate) fn cannot_open(&self, key: &str, e: Error) {
        self.pending.fail(key);
        let shown = doc::host_file_path(key).unwrap_or(key);
        match e {
            Error::Core(Core::NotFound(_)) => {
                self.cannot(&format!("open {shown}"), "not in this vault")
            }
            e => self.cannot(&format!("open {shown}"), e),
        }
    }

    /// Open File…: anything, from anywhere. A file inside this vault opens as a vault tab; one
    /// from outside opens as a loose tab in this window, marked as being from outside it.
    pub fn open_file_dialog(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder().title("Open File").build();
        // A remote vault's root is a path on its host: this machine's chooser cannot start there,
        // and a local pick under the same path is not in the vault, so it always opens loose.
        if let Some(vault) = self.vault().filter(|v| !v.is_remote()) {
            dialog.set_initial_folder(Some(&gio::File::for_path(vault.root())));
        }
        dialog.open(
            Some(&self.window),
            gio::Cancellable::NONE,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |result| {
                    // A dismissed chooser is an error here, and not one worth a toast.
                    let Some(path) = result.ok().and_then(|file| file.path()) else {
                        return;
                    };
                    let local = app.vault().filter(|v| !v.is_remote());
                    let key = match local.and_then(|v| path.strip_prefix(v.root()).ok()) {
                        Some(rel) => rel.to_string_lossy().into_owned(),
                        None => path.to_string_lossy().into_owned(),
                    };
                    app.open_path(&key);
                }
            ),
        );
    }

    /// Watch a local loose file, which the vault worker cannot report outside its tree.
    /// Changes reach PDF/image tabs through changed_on_disk; vault files return None.
    pub(crate) fn watch_loose(self: &Rc<Self>, key: &str, path: &Path) -> Option<gio::FileMonitor> {
        if !doc::is_loose_key(key) || doc::host_file_path(key).is_some() {
            return None;
        }
        let key = key.to_string();
        editor::watch_file(
            path,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move || app.changed_on_disk(&key)
            ),
        )
    }

    /// The vault `tab`'s language document opens on: this window's for its own files, and for
    /// code from outside it, a dependency its server may have named (`lang::join`). Remote host
    /// dependencies use their own key; ordinary loose tabs read this machine's disk.
    pub(crate) fn language_vault(&self, tab: &Tab) -> Option<Arc<Vault>> {
        let vault = self.vault()?;
        let loose_code = tab.flavour() == Flavour::Code && !vault.is_remote();
        (!doc::is_loose_key(&tab.rel()) || loose_code || doc::host_file_path(&tab.rel()).is_some())
            .then(|| vault.clone())
    }

    pub(crate) fn mark_loose(&self, page: &adw::TabPage, key: &str) {
        if self.vault.is_some() && doc::is_loose_key(key) {
            page.set_indicator_icon(Some(&gio::ThemedIcon::new("document-open-symbolic")));
            page.set_indicator_tooltip(if doc::host_file_path(key).is_some() {
                "On remote host"
            } else {
                "Outside this vault"
            });
        }
    }

    /// Wire a freshly opened tab into the window.
    fn adopt(self: &Rc<Self>, tab: Rc<Tab>, how: Opened) {
        // Ctrl+scroll zooms the document, as it zooms a PDF page, through the same step and the
        // same readout. On the view rather than on the window: a window-level controller would
        // have to work out which tab the pointer is over and would race the PDF's own, while this
        // one only ever sees a text tab. Bubble phase, ahead of the scrolled window's controller,
        // which is the order `pdfview` relies on for the same reason.
        zoom_on_wheel(
            &tab.view,
            gtk::PropagationPhase::Bubble,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |out| app.set_zoom(stepped_zoom(app.zoom.get(), out))
            ),
        );

        tab.connect_autosave(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |tab| app.save_tab(tab, false)
        ));
        tab.connect_edited(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |tab| {
                lang::changed(tab);
                app.queue_refresh(tab);
                if app.speaks_for(tab) {
                    app.sync_outline();
                }
            }
        ));
        tab.connect_banner(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |tab| app.answer_banner(tab)
        ));
        tab.connect_follow(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_| app.go_to_definition()
        ));
        tab.connect_cursor(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |tab| {
                app.sync_scroll(tab);
                // Typing carries the caret along without taking the Outline pane with it: the
                // list stays where the reader left it until the caret is moved.
                if app.is_active(tab) && tab.caret_moved() {
                    app.follow_outline();
                }
                if app.is_active(tab) {
                    app.follow_references();
                }
                // The goals are about the caret too, typing included: a tactic typed changes them.
                if app.is_active(tab) && tab.lang.support().is_some_and(|s| s.goals) {
                    app.refresh_goals();
                }
            }
        ));
        // The selection's count, once a turn however many marks moved: a column of carets moves
        // one each, and taking a caret away deletes its marks.
        let counting = Rc::new(Cell::new(false));
        let count = Rc::new(glib::clone!(
            #[weak(rename_to = app)]
            self,
            #[weak]
            tab,
            move || {
                if counting.replace(true) {
                    return;
                }
                let counting = counting.clone();
                // Ahead of the frame, and of GTK's own idles, which lay out a long note for
                // seconds after it is selected whole.
                glib::idle_add_local_full(glib::Priority::HIGH_IDLE, move || {
                    counting.set(false);
                    if app.is_active(&tab) {
                        app.sync_selection();
                    }
                    glib::ControlFlow::Break
                });
            }
        ));
        tab.buffer.connect_mark_set({
            let count = count.clone();
            move |_, _, _| count()
        });
        tab.buffer.connect_mark_deleted(move |_, _| count());
        // The chrome hides on the keystroke itself, not on the debounce that follows it.
        tab.buffer.connect_changed(glib::clone!(
            #[weak(rename_to = app)]
            self,
            #[weak]
            tab,
            move |_| app.on_edit(&tab)
        ));
        // A key or a click that takes the caret far at once leaves a place to come back to.
        if let Some(view) = tab.view.downcast_ref::<multicaret::View>() {
            view.on_leap(glib::clone!(
                #[weak(rename_to = app)]
                self,
                #[weak]
                tab,
                move |from, _| app
                    .leaped(&Doc::Text(tab.clone()), &tab.buffer.iter_at_offset(from))
            ));
        }

        // A click in the marks column toggles a breakpoint, where the file is one a debugger runs.
        sourceview5::prelude::ViewExt::connect_line_mark_activated(
            &tab.view,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                #[weak]
                tab,
                move |_, at, button, _, presses| {
                    if button == gdk::BUTTON_PRIMARY && presses == 1 {
                        app.toggle_breakpoint(&tab, at.line().max(0) as u32);
                    }
                }
            ),
        );
        self.debugger.place(&tab);

        // A file inside the vault gets a document on the language layer; a loose note outlines
        // itself, and loose code joins the server that named it, if one did.
        lang::attach(
            &tab,
            self.language_vault(&tab),
            lang::Hooks {
                on_symbols: Rc::new(glib::clone!(
                    #[weak(rename_to = app)]
                    self,
                    move |tab: &Rc<Tab>| {
                        // The pinned title is drawn from the same symbols, so it is redrawn
                        // whether or not this tab is the one being looked at.
                        tab.update_sticky();
                        if app.speaks_for(tab) {
                            app.sync_outline();
                        }
                    }
                )),
                on_support: Rc::new(glib::clone!(
                    #[weak(rename_to = app)]
                    self,
                    move |tab: &Rc<Tab>| {
                        if app.is_active(tab) {
                            app.sync_code_pane();
                            app.follow_references();
                        }
                    }
                )),
                on_starting: Rc::new(glib::clone!(
                    #[weak(rename_to = app)]
                    self,
                    move |tab: &Rc<Tab>| {
                        if app.is_active(tab) {
                            app.statusbar
                                .set_starting(tab.lang.doc.starting().as_deref());
                        }
                    }
                )),
                evaluate: Rc::new(glib::clone!(
                    #[weak(rename_to = app)]
                    self,
                    #[upgrade_or_default]
                    move |tab: &Rc<Tab>, expression: String| {
                        use accent_api::debug::{Context, debuggable};
                        if !debuggable(&lang::language_id(tab)) {
                            return None;
                        }
                        app.debug_evaluate(expression, Context::Hover)
                    }
                )),
            },
        );

        // A pasted or dropped image is written into the vault, which a loose note has none of.
        if tab.flavour().is_note() && self.vault().is_some() && !doc::is_loose_key(&tab.rel()) {
            self.wire_attachments(&tab);
        }
        // A code tab's right-click menu holds its server's actions at the caret, which the press
        // puts where it was; a note's holds its spelling.
        if tab.flavour() == Flavour::Code {
            tab.view.set_extra_menu(Some(&fixes::menu()));
        }
        self.offer_show_in_pdf(&tab);

        // Nothing else watches a loose file: the vault's worker only reports on its own tree.
        if doc::is_loose_key(&tab.rel()) && doc::host_file_path(&tab.rel()).is_none() {
            tab.watch_file(glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |tab| app.file_changed(tab)
            ));
        }
        self.mark_loose(&tab.page, &tab.rel());
        // A package manager's or a toolchain's file is theirs, not the user's to edit, however it
        // was opened and before any server answers; a sibling crate a path dependency names is not.
        if doc::host_file_path(&tab.rel()).is_some() {
            tab.view.set_editable(false);
            tab.show_alert(Alert::Host);
        } else if doc::is_loose_key(&tab.rel()) && accent_core::path::managed(Path::new(&tab.rel()))
        {
            tab.view.set_editable(false);
            tab.show_alert(Alert::Installed);
        }
        let page = tab.page.clone();
        if doc::host_file_path(&tab.rel()).is_none() {
            self.fetch_head(&tab);
        }
        self.watch_folder_of(&tab.rel());
        self.docs.borrow_mut().push(Doc::Text(tab.clone()));
        self.landed(&page, how);
        // Taken out first: the borrow of an `if let`'s scrutinee lasts through its body, and the
        // restore's `put_back` asks what else is still waiting.
        let waiting = self.pending.take_work(&tab.rel());
        if let Some(waiting) = waiting {
            (waiting.run)(self, &tab);
        }
    }

    /// Choose restored pane placement for `key` while available, otherwise the active pane.
    /// Worker-read text tabs need this lookup on arrival instead of moving after construction.
    pub(crate) fn tabs_for(&self, key: &str) -> adw::TabView {
        let placed = self.pending.take_pane(key);
        match placed.filter(|pane| self.panes.borrow().iter().any(|p| Rc::ptr_eq(p, pane))) {
            Some(pane) => pane.tabs.clone(),
            None => self.tabs(),
        }
    }

    /// Select a new page unless [`App::put_back`] owns its restored selection.
    ///
    /// Selection normally triggers `sync_active`. A pane's first page is selected before its
    /// document enters `docs`, so sync it manually when no new notification fires.
    pub fn select_new_page(self: &Rc<Self>, page: &adw::TabPage, how: Opened) {
        let Some(pane) = self.pane_of(page) else {
            return;
        };
        match pane.tabs.selected_page().as_ref() == Some(page) {
            true => {
                // Selected by being the first page added, before it had a document, so the notify
                // could not tell whose it was: anything but a restored tab is the reader opening
                // a note into an empty pane, which makes that pane theirs (`App::put_back`).
                if how != Opened::Restored {
                    self.reader_in(&pane);
                }
                self.sync_active();
            }
            false if how == Opened::Restored => {}
            false => pane.tabs.set_selected_page(page),
        }
    }

    /// Give a tab picked in the palette the keyboard, wherever the palette gave it back to.
    pub(crate) fn focus_picked(&self, page: &adw::TabPage, how: Opened) {
        if how == Opened::Picked
            && let Some(pane) = self.pane_of(page)
        {
            self.focus_document(&pane);
        }
    }

    /// Run `f` on key's text tab, opening it first if needed.
    /// Worker opens may call later through [`App::adopt`]. Non-text results discard queued work
    /// with the requested `what` reason, preventing callbacks from reaching a later unrelated open.
    pub fn with_tab(
        self: &Rc<Self>,
        key: &str,
        how: Opened,
        what: &str,
        f: impl FnOnce(&Rc<App>, &Rc<Tab>) + 'static,
    ) {
        if let Some(tab) = self.tab_for(key) {
            self.reveal_page(&tab.page);
            self.focus_picked(&tab.page, how);
            return f(self, &tab);
        }
        let waiting = pending::Waiting {
            what: (how != Opened::Restored)
                .then(|| format!("{what} {}", doc::host_file_path(key).unwrap_or(key))),
            run: Box::new(f),
        };
        self.pending.wait(key, waiting);
        self.open_as(key, how);
    }

    /// Discard pending text-tab work for `key` with its request reason when the file opens as another kind.
    fn drop_awaiting(&self, key: &str, why: &str) {
        let waiting = self.pending.take_work(key);
        if let Some(what) = waiting.and_then(|waiting| waiting.what) {
            self.cannot(&what, why);
        }
    }
}

/// What a diagram or a notebook is to show once it is open: see [`App::reveal_in_diagram`] and
/// [`App::open_cell`]; or what is to be done with any document once it is, a print or an export
/// from a file's menu (`App::make`).
pub enum Reveal {
    /// A notebook's cell, by its place, and the 1-based line and column in it, where Back opens a
    /// closed notebook.
    NotebookCell(usize, i32, i32),
    Diagram(Shows<crate::diagram::DiagramTab>),
    NotebookHit(std::ops::Range<usize>),
    NotebookFilePos(accent_api::Pos),
    Then(OnDoc),
    /// The 1-based line and column a terminal editor opens the file at ([`App::edit_at`]).
    Line(u32, Option<u32>),
}

/// A notebook opened as JSON needs its indexed source offset translated to the JSON spelling.
fn select_text_range(tab: &Tab, bytes: std::ops::Range<usize>) {
    let text = tab.text();
    let bytes = if accent_core::path::is_notebook(&tab.rel()) {
        accent_core::notebook::read(&tab.rel(), &text)
            .ok()
            .and_then(|notebook| notebook.json_hit(bytes.clone()))
            .unwrap_or(bytes)
    } else {
        bytes
    };
    if let Some(chars) = char_range(&text, bytes) {
        tab.goto_range(chars);
    }
}

/// What a [`Reveal`] does to the tab it waited for.
type Shows<T> = Box<dyn FnOnce(&Rc<App>, &Rc<T>)>;

/// What [`Reveal::Then`] does with the document it waited for.
pub(crate) type OnDoc = Box<dyn FnOnce(&Rc<App>, &Doc)>;

/// Show the label of `tab` whose line in its labels' text holds byte `at`, if there still is one.
fn reveal_label(tab: &Rc<crate::diagram::DiagramTab>, at: usize) {
    if let Some((page, cell)) = accent_drawio::text::label_at(&tab.file(), at) {
        tab.reveal_cell(page, &cell);
    }
}

/// Choose text editor styling: an org file is a note in org's syntax; a table uses column colours
/// instead of csv.lang token colours. A TSV is read with tabs whatever its first line holds, where
/// a CSV's delimiter is sniffed.
fn flavour_of(key: &str) -> Flavour {
    if accent_core::path::is_org(key) {
        return Flavour::Note(Syntax::Org);
    }
    match accent_core::path::file_type(key) {
        accent_core::path::FileType::Table => {
            let tsv = key
                .rsplit_once('.')
                .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("tsv"));
            Flavour::Csv(tsv.then_some(b'\t'))
        }
        _ => Flavour::Code,
    }
}

/// The flavour a text tab on `key` opens in, `None` for a file that opens as another document.
pub(crate) fn text_flavour(key: &str) -> Option<Flavour> {
    match doc::kind_of(key) {
        Kind::Note => Some(Flavour::Note(Syntax::Markdown)),
        Kind::Text => Some(flavour_of(key)),
        Kind::Image | Kind::Pdf | Kind::Diagram | Kind::Notebook => None,
    }
}

/// Whether a document open on `from` is the same kind of page on `to`, which a rename then
/// changes in place: a note in markdown or org, code in any language, a table, or the same kind
/// of other document. A note's page and a code page are built differently, so a rename between
/// them opens the file anew.
pub(crate) fn same_page(from: &str, to: &str) -> bool {
    match (text_flavour(from), text_flavour(to)) {
        (Some(a), Some(b)) => std::mem::discriminant(&a) == std::mem::discriminant(&b),
        (None, None) => doc::kind_of(from) == doc::kind_of(to),
        _ => false,
    }
}

/// A byte count as a person reads it, in the decimal units GNOME shows in Files.
fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["bytes", "kB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1000.0 && unit + 1 < UNITS.len() {
        size /= 1000.0;
        unit += 1;
    }
    match unit {
        0 => format!("{bytes} bytes"),
        _ => format!("{size:.1} {}", UNITS[unit]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rename_keeps_the_page_only_within_its_kind() {
        // Another syntax of the same page: a language, a delimiter, org for markdown.
        assert!(same_page("a.txt", "a.py"));
        assert!(same_page("a.csv", "a.tsv"));
        assert!(same_page("a.md", "a.org"));
        assert!(same_page("a.png", "a.jpg"));
        // A note's page and a code page are built differently, and a PDF is no text at all.
        assert!(!same_page("a.md", "a.txt"));
        assert!(!same_page("a.txt", "a.csv"));
        assert!(!same_page("a.md", "a.pdf"));
        assert!(!same_page("a.pdf", "a.png"));
    }
}
