//! Dispatch file opens, read on workers and adopt text, image, diff or status tabs into panes.
//! PDF opening lives in `pdf::window`; Replace All lives in `replace`.

use super::*;
use accent_api::Error;
use accent_core::Error as Core;

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
            self.awaiting.borrow_mut().remove(key);
            // Deleted since the session named it, or never in this vault: the vault has answered
            // the same way for both, and this machine's disk cannot tell them apart for a remote.
            return self.cannot(&format!("open {key}"), "not in this vault");
        };
        // A note that is already open keeps whatever it is: looking at a real tab again does not
        // demote it, and looking at the preview again does not promote it.
        if let Some(doc) = self.doc_for(&key) {
            self.drop_awaiting(&key, "not a text file");
            self.reveal_page(doc.page());
            return self.focus_picked(doc.page(), how);
        }
        let kind = doc::kind_of(&key);
        // Only text becomes a `Tab`, so anything else has nothing for a waiting closure to run on.
        if !matches!(kind, Kind::Note | Kind::Text) {
            self.drop_awaiting(&key, "not a text file");
        }
        match kind {
            Kind::Note => self.open_text(&key, &path, Flavour::Note, how),
            Kind::Image => self.open_image(&key, &path, how),
            Kind::Pdf => self.open_pdf(&key, &path, how),
            Kind::Diagram => self.open_diagram(&key, &path, how),
            Kind::Text => self.open_text(&key, &path, flavour_of(&key), how),
        }
    }

    /// Adopt an arriving preview, replacing the pane's previous one and applying keep or pin state.
    ///
    /// Insert the new tab before closing the old one so the pane cannot close while briefly
    /// empty. Earlier tree double-clicks keep the arriving tab; launch tabs retain named order.
    pub(crate) fn mark_opened(self: &Rc<Self>, page: &adw::TabPage, how: Opened) {
        // A label or a page a diagram waits to show, which a file that turned out to be text
        // drops.
        if let Some(doc) = self.doc_for_page(page) {
            let reveal = self.revealing.borrow_mut().remove(&doc.key());
            if let (Some(reveal), Doc::Diagram(tab)) = (reveal, &doc) {
                reveal(self, tab);
            }
        }
        // A double click in the Files tree whose second press came before the tab did.
        let keep = self
            .doc_for_page(page)
            .is_some_and(|doc| self.keep_on_landing.borrow_mut().remove(&doc.key()));
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
        if let Some(old) = pane.set_preview(page) {
            pane.tabs.close_page(&old);
        }
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
        // A loose file has no vault to ask, so it still reads its own absolute path.
        let vault = self.vault().filter(|_| !doc::is_loose_key(key)).cloned();
        let (key, path) = (key.to_string(), path.to_path_buf());
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let read = crate::work::off_thread("reader", {
                let key = key.clone();
                move || {
                    // A slow host, for the double clicks of `ACCENT_BENCH_TABS=tree:`.
                    #[cfg(feature = "bench")]
                    if let Some(ms) = std::env::var("ACCENT_BENCH_SLOW_READ")
                        .ok()
                        .and_then(|ms| ms.parse().ok())
                    {
                        std::thread::sleep(std::time::Duration::from_millis(ms));
                    }
                    match vault {
                        Some(vault) => vault.read_text(&key),
                        None => accent_core::fs::read_text(&path).map_err(Into::into),
                    }
                }
            })
            .await;
            let Some(app) = weak.upgrade() else { return };
            // A second open while the first read was in flight — two clicks on the same row, or
            // the command line naming a note the session restores: the tab exists now, and comes
            // to the front as an open file does.
            if let Some(doc) = app.doc_for(&key) {
                if how != Opened::Restored {
                    app.reveal_page(doc.page());
                    app.focus_picked(doc.page(), how);
                }
                return;
            }
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
        let text = match read {
            Ok(accent_core::fs::Read::Text(text)) => text,
            Ok(accent_core::fs::Read::Binary { size }) => {
                return self.open_status(
                    key,
                    "Binary File",
                    &format!("{} is not text, so there is nothing to edit.", human(size)),
                    how,
                );
            }
            Ok(accent_core::fs::Read::TooLarge { size }) => {
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
            Err(e) => {
                self.awaiting.borrow_mut().remove(key);
                return self.cannot_open(key, e);
            }
        };
        // An `.xml` that draw.io wrote is a diagram, whatever its name says: known by its bytes.
        // A search hit in it waits for the diagram instead of the text tab it asked for.
        if accent_core::path::holds_diagram(key, &text.text) {
            match self.revealing.borrow().contains_key(key) {
                true => drop(self.awaiting.borrow_mut().remove(key)),
                false => self.drop_awaiting(key, "a diagram, not a text file"),
            }
            return self.open_diagram_text(key, text, how);
        }
        let prefs = self.prefs();
        let tab = editor::open(
            &self.root(),
            key,
            text,
            flavour,
            &self.tabs_for(key),
            &prefs,
        );
        self.adopt(tab, how);
        self.sync_conflict_banner(key, None);
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
        let image = self.adopt_viewer(Doc::Image, key, &scroller, "image-x-generic-symbolic", how);
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
                Err(e) => app.gone(image.key(), &image.page, e),
            }
        });
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
                look::show(&from, original, look, inverted, scale, zoom)
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
                    // The size a zoomed picture asks for is worked out from its paintable, so a
                    // file of another size would be drawn at its own while the readout kept the
                    // old percentage. Asked again of this one, so the zoom means the same thing
                    // either side of a reload.
                    zoom::set_image_zoom(&picture, image.zoom.get());
                    let own = (shown.texture.width(), shown.texture.height());
                    image.size.set(Some(shown.size.unwrap_or(own)));
                    image.animated.set(shown.animated);
                    app.sync_details();
                    app.sync_image_invert();
                    *image.image.borrow_mut() = Some((path, shown.original));
                    app.show_image(&image, None);
                }
                // A file that never showed goes the way one that never arrived does; one that
                // did keeps its last contents up.
                Some(Err(e)) if picture.paintable().is_none() => app.gone(
                    image.key(),
                    &image.page,
                    Core::Invalid(e.to_string()).into(),
                ),
                Some(Err(e)) => app.cannot("reload", e),
                None => app.cannot("reload", "the image worker stopped"),
            }
        });
    }

    /// Grey out Invert Image Colours over an animation, which is never recoloured, once the image
    /// in front has been read.
    pub(crate) fn sync_image_invert(&self) {
        let action = self.window.lookup_action("image-invert");
        if let Some(action) = action.and_downcast::<gio::SimpleAction>() {
            action.set_enabled(!self.active_animation());
        }
    }

    /// Whether the tab in front shows an animation.
    pub(crate) fn active_animation(&self) -> bool {
        matches!(self.active_doc(), Some(Doc::Image(image)) if image.animated.get())
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
                move || match vault {
                    Some(vault) => vault.fetch_with(&key, &|done, total| bytes.set(done, total)),
                    None => Ok(path),
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
    /// sentence and at most one button).
    pub(crate) fn open_status(self: &Rc<Self>, key: &str, title: &str, body: &str, how: Opened) {
        self.drop_awaiting(key, &title.to_lowercase());
        let key = key.to_string();
        let status = adw::StatusPage::builder()
            .icon_name("dialog-warning-symbolic")
            .title(title)
            .description(body)
            .build();
        // A file on the host is on no disk here for a file manager to show, so there the one
        // button is the copy that puts it on one.
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
        self.adopt_viewer(Doc::Status, &key, &status, "dialog-warning-symbolic", how);
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
            Kind::Note => Flavour::Note,
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
        self.docs.borrow_mut().push(Doc::Diff(tab.clone()));
        let how = match restored {
            true => Opened::Restored,
            false => Opened::Preview,
        };
        self.select_new_page(&tab.page, how);
        self.mark_opened(&tab.page, how);
        tab
    }

    /// Put a tab with no buffer into the window: the shared half of [`App::open_image`] and
    /// [`App::open_status`].
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
        page.set_tooltip(&fileops::display_path(&self.root(), key));
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
            Some(sidebar::Target::Range(bytes)) if doc::kind_of(rel) == Kind::Diagram => {
                self.open_label(rel, bytes.start)
            }
            // Only its bytes say whether an `.xml` is a diagram: the hit lands on the label if it
            // is one and on the text if it is not, whichever tab it opens as.
            Some(sidebar::Target::Range(bytes)) if accent_core::path::may_be_diagram(rel) => {
                match self.doc_for(rel) {
                    Some(Doc::Diagram(_)) => self.open_label(rel, bytes.start),
                    Some(_) => self.select_when_open(rel, sidebar::Target::Range(bytes)),
                    None => {
                        let at = bytes.start;
                        self.revealing.borrow_mut().insert(
                            rel.to_string(),
                            Box::new(move |_, tab| reveal_label(tab, at)),
                        );
                        self.select_when_open(rel, sidebar::Target::Range(bytes));
                    }
                }
            }
            Some(at) => self.select_when_open(rel, at),
            None => self.open_preview(rel),
        }
    }

    /// Open diagram `rel` at the cell matching byte offset `at` in indexed label lines.
    /// Reveal its page and cell once a pending worker read lands ([`mark_opened`](Self::mark_opened)).
    fn open_label(self: &Rc<Self>, rel: &str, at: usize) {
        self.reveal_in_diagram(rel, Box::new(move |_, tab| reveal_label(tab, at)));
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
        self.reveal_in_diagram(rel, Box::new(show));
    }

    /// Show `reveal` in diagram `rel`, now if it is open, else once a worker has read it
    /// ([`mark_opened`](Self::mark_opened)).
    fn reveal_in_diagram(self: &Rc<Self>, rel: &str, reveal: Reveal) {
        if let Some(Doc::Diagram(tab)) = self.doc_for(rel) {
            self.reveal_page(&tab.page);
            return reveal(self, &tab);
        }
        self.revealing.borrow_mut().insert(rel.to_string(), reveal);
        self.open_preview(rel);
    }

    /// Put the caret over `at` once `rel` has a tab, whoever opened it: a search hit, a tag or a
    /// followed link. A tag names itself rather than a place, because only the note knows where
    /// it writes it.
    fn select_when_open(self: &Rc<Self>, rel: &str, at: sidebar::Target) {
        self.with_tab(rel, Opened::Preview, "open", move |_, tab| match &at {
            sidebar::Target::Range(bytes) => {
                if let Some(chars) = char_range(&tab.text(), bytes.clone()) {
                    tab.goto_range(chars);
                }
            }
            sidebar::Target::Tag(name) => tab.goto_tag(name),
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
            if let Some(rel) = self.active_key() {
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
        let what = match anchor.starts_with('^') {
            true => "block",
            false => "heading",
        };
        self.toast(&format!("No {what} {anchor} in {name}"));
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

    /// Resolve the opening key and read path using the vault's lexical containment rule.
    ///
    /// Reject escaping wikilinks here, since note content reaches this entry point directly.
    /// Avoid canonicalisation so intentionally indexed directory symlinks still work. Existence
    /// checks stay on the reader's worker to avoid a remote round trip on the main thread;
    /// [`cannot_open`](Self::cannot_open) reports failure.
    pub(crate) fn locate(&self, key: &str) -> Option<(String, PathBuf)> {
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
        self.keep_on_landing.borrow_mut().remove(key);
        match e {
            Error::Core(Core::NotFound(_)) => {
                self.cannot(&format!("open {key}"), "not in this vault")
            }
            e => self.cannot(&format!("open {key}"), e),
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

    /// Mark loose-file tabs and watch their files because the vault worker cannot report outside its tree.
    /// Changes reach PDF/image tabs through changed_on_disk; vault files return None.
    pub(crate) fn watch_loose(self: &Rc<Self>, key: &str, path: &Path) -> Option<gio::FileMonitor> {
        if !doc::is_loose_key(key) {
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
    /// code from outside it, a dependency its server may have named (`lang::join`). Not a remote
    /// vault's: its dependencies are on the host, and a loose tab reads this machine's disk.
    pub(crate) fn language_vault(&self, tab: &Tab) -> Option<Arc<Vault>> {
        let vault = self.vault()?;
        let loose_code = tab.flavour() == Flavour::Code && !vault.is_remote();
        (!doc::is_loose_key(&tab.rel()) || loose_code).then(|| vault.clone())
    }

    pub(crate) fn mark_loose(&self, page: &adw::TabPage, key: &str) {
        if self.vault.is_some() && doc::is_loose_key(key) {
            page.set_indicator_icon(Some(&gio::ThemedIcon::new("document-open-symbolic")));
            page.set_indicator_tooltip("Outside this vault");
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
                if app.is_active(tab) {
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
                        if app.is_active(tab) {
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
                                .set_starting(tab.lang.starting.borrow().as_deref());
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
        self.offer_show_in_pdf(&tab);

        // Nothing else watches a loose file: the vault's worker only reports on its own tree.
        if doc::is_loose_key(&tab.rel()) {
            tab.watch_file(glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |tab| app.file_changed(tab)
            ));
        }
        self.mark_loose(&tab.page, &tab.rel());
        // A package manager's or a toolchain's file is theirs, not the user's to edit, however it
        // was opened and before any server answers; a sibling crate a path dependency names is not.
        if doc::is_loose_key(&tab.rel()) && accent_core::path::managed(Path::new(&tab.rel())) {
            tab.view.set_editable(false);
            tab.show_alert(Alert::Installed);
        }
        let page = tab.page.clone();
        self.fetch_head(&tab);
        self.watch_folder_of(&tab.rel());
        self.docs.borrow_mut().push(Doc::Text(tab.clone()));
        self.select_new_page(&page, how);
        self.mark_opened(&page, how);
        self.save_session_soon();
        // Taken out first: the borrow of an `if let`'s scrutinee lasts through its body, and the
        // restore's `put_back` asks what else is still waiting.
        let waiting = self.awaiting.borrow_mut().remove(&tab.rel());
        if let Some(waiting) = waiting {
            (waiting.run)(self, &tab);
        }
    }

    /// Choose restored pane placement for `key` while available, otherwise the active pane.
    /// Worker-read text tabs need this lookup on arrival instead of moving after construction.
    pub(crate) fn tabs_for(&self, key: &str) -> adw::TabView {
        let placed = self.placing.borrow_mut().remove(key);
        match placed
            .and_then(|pane| pane.upgrade())
            .filter(|pane| self.panes.borrow().iter().any(|p| Rc::ptr_eq(p, pane)))
        {
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
    fn focus_picked(&self, page: &adw::TabPage, how: Opened) {
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
        let waiting = Waiting {
            what: (how != Opened::Restored).then(|| format!("{what} {key}")),
            run: Box::new(f),
        };
        self.awaiting.borrow_mut().insert(key.to_string(), waiting);
        self.open_as(key, how);
    }

    /// Discard pending text-tab work for `key` with its request reason when the file opens as another kind.
    fn drop_awaiting(&self, key: &str, why: &str) {
        let waiting = self.awaiting.borrow_mut().remove(key);
        if let Some(what) = waiting.and_then(|waiting| waiting.what) {
            self.cannot(&what, why);
        }
    }
}

/// What a diagram is to show once it is open: see [`App::reveal_in_diagram`].
pub type Reveal = Box<dyn FnOnce(&Rc<App>, &Rc<crate::diagram::DiagramTab>)>;

/// Show the label of `tab` whose line in its labels' text holds byte `at`, if there still is one.
fn reveal_label(tab: &Rc<crate::diagram::DiagramTab>, at: usize) {
    if let Some((page, cell)) = accent_drawio::text::label_at(&tab.file(), at) {
        tab.reveal_cell(page, &cell);
    }
}

/// Choose text editor styling; a table uses column colours instead of csv.lang token colours. A
/// TSV is read with tabs whatever its first line holds, where a CSV's delimiter is sniffed.
fn flavour_of(key: &str) -> Flavour {
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
