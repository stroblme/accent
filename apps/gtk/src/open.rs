//! Opening a file into a tab: the only door into one. What a key opens as, the worker read, the
//! PDF, image, diff and status tabs, and adopting the result into a pane.

use super::*;
use accent_core::pdf::PageEdit;

/// Why a tab was opened, which decides whether it stays.
///
/// A tab opened by browsing — a click in the sidebar tree, a search hit, a Git row, a wikilink
/// followed — is a `Preview`: the next such open closes it and takes its place, so clicking down
/// a list of notes to see what is in them leaves one tab rather than twenty. Anything the reader
/// named is `Kept`: the palette, Open File…, a drop, a rename and the command line, where the
/// file was asked for by name and the tab is meant to stay. A preview tab becomes a kept one the
/// moment it is edited, its own tab is double-clicked, or it is moved to another pane, all three
/// being the reader saying they want to keep it.
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
}

impl App {
    /// Open anything, from the tree, the palette, a link, a drop, the session or the command
    /// line. This is the only door into a tab.
    ///
    /// What a file opens as is decided by its name first, and then by its bytes when the name
    /// says "text": a `.png` that is really random bytes is still an image tab, but a `.py` full
    /// of NULs is a status page rather than a screen of garbage.
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
            return self.reveal_page(doc.page());
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

    /// A preview tab has arrived: it replaces whichever tab this pane was previewing before. A
    /// pinned one is pinned here.
    ///
    /// The old tab goes after the new one is in place, so the pane never stands empty and closes
    /// itself out from under the note arriving in it.
    pub(crate) fn mark_opened(self: &Rc<Self>, page: &adw::TabPage, how: Opened) {
        if how == Opened::Pinned {
            return self.set_pinned(page, true);
        }
        if how != Opened::Preview {
            return;
        }
        let Some(pane) = self.pane_of(page) else {
            return;
        };
        if let Some(old) = pane.set_preview(page) {
            pane.tabs.close_page(&old);
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

    /// A text file in an editor tab, unless its bytes say it is not one after all.
    ///
    /// The read happens on a worker thread, so opening a note on a remote vault does not hold the
    /// window for the round trip — measured at ~60 ms to the host this was developed against,
    /// which is four frames. Locally it lands in the same turn of the loop and nothing changes.
    fn open_text(self: &Rc<Self>, key: &str, path: &Path, flavour: Flavour, how: Opened) {
        // A loose file has no vault to ask, so it still reads its own absolute path.
        let vault = self.vault().filter(|_| !doc::is_loose_key(key)).cloned();
        let (key, path) = (key.to_string(), path.to_path_buf());
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let read = crate::work::off_thread("reader", {
                let key = key.clone();
                move || match vault {
                    Some(vault) => vault.read_text(&key),
                    None => accent_core::fs::read_text(&path),
                }
            })
            .await;
            let Some(app) = weak.upgrade() else { return };
            // Two clicks on the same row while the first read was in flight: the tab exists now.
            if app.doc_for(&key).is_some() {
                return;
            }
            // A worker that panicked is one more way for the read to fail, and is said as the others
            // are: the open named in a toast, and whatever was waiting on the tab let go with it.
            let read = read.unwrap_or_else(|| Err(std::io::Error::other("the worker stopped")));
            app.adopt_text(&key, read, flavour, how);
        });
    }

    /// What [`open_text`](Self::open_text) does once the bytes are in hand.
    fn adopt_text(
        self: &Rc<Self>,
        key: &str,
        read: std::io::Result<accent_core::fs::Read>,
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
                        "{} is over the {} MiB accent will read into an editor.",
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
        if doc::file_name(key).to_ascii_lowercase().ends_with(".xml") && diagram::sniff(&text.text)
        {
            self.drop_awaiting(key, "a diagram, not a text file");
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

    /// A PDF, in the reader.
    fn open_pdf(self: &Rc<Self>, key: &str, path: &Path, how: Opened) {
        let place = self
            .vault()
            .and_then(|v| v.session().pdf.get(key).copied())
            .unwrap_or_default();
        let pdf = pdftab::open(
            path,
            key,
            doc::file_name(key),
            &fileops::display_path(&self.root(), key),
            &self.tabs_for(key),
            place,
        );
        pdf.set_drawing_config(self.config.borrow().drawing.clone());
        pdf.connect_zoom(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_| {
                app.refresh_zoom();
                app.save_session_soon();
            }
        ));
        // A page jump, a link followed, an outline row: the reader is leaving a place, and the
        // pane's history is where that goes. Fired before the view moves.
        pdf.connect_jump(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |pdf| app.mark_page(&pdf.page)
        ));
        pdf.connect_page(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_| {
                // Edge-triggered: `PdfView` reports the page under the middle of the viewport
                // only when it changes, so this is once per page boundary crossed, not once per
                // scrolled pixel, and needs no debounce of its own.
                app.sync_status();
                app.save_session_soon();
            }
        ));
        pdf.connect_outline(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_| app.sync_outline()
        ));
        pdf.connect_opened(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |pdf| {
                app.sync_opening();
                app.sync_outline();
                // The page count is known now, so the readouts have something to say at last.
                app.sync_status();
                if let Some(pane) = app.pane_of(&pdf.page) {
                    pane.find.refresh_count();
                }
                // And the pages are there to paint the notes' highlights onto.
                app.sync_pdf_links(pdf);
            }
        ));
        pdf.connect_mode(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_| {
                app.sync_status();
                app.sync_history();
            }
        ));
        pdf.connect_history(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_| app.sync_history()
        ));
        pdf.connect_note(glib::clone!(
            #[weak(rename_to = app)]
            self,
            // A zero-length range: the caret goes to the `[[`, nothing is selected.
            move |rel, at| app.open_note_at(rel, Some(sidebar::Target::Range(at..at)))
        ));
        pdf.connect_save_failed(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |pdf, why| {
                let name = doc::file_name(&pdf.key()).to_string();
                app.cannot(&format!("save {name}"), why);
            }
        ));
        pdf.connect_saved(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |pdf| app.push_pdf(pdf)
        ));
        pdf.connect_export(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |pdf, result| app.exported(pdf, result)
        ));
        // A page edit does not rewrite the notes that name pages by number (NOTEPAD), so the
        // reader is told when it left some of them pointing at other pages.
        pdf.connect_links_moved(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_, moved| app.toast(&match moved {
                1 => "A highlight in a note now points at another page".to_string(),
                n => format!("{n} highlights in notes now point at other pages"),
            })
        ));
        pdf.connect_choice(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_, tool, choice| app.pdf_choice(tool, choice)
        ));
        pdf.connect_matches(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |pdf| {
                if let Some(pane) = app.pane_of(&pdf.page) {
                    pane.find.set_matches_text(&pdf.matches_label());
                }
            }
        ));
        pdf.connect_uri(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |uri| {
                let launcher = gtk::UriLauncher::new(uri);
                launcher.launch(Some(&app.window), gio::Cancellable::NONE, |_| {});
            }
        ));
        let page = pdf.page.clone();
        self.mark_loose(&page, key);
        let reader = Rc::downgrade(&pdf);
        self.docs.borrow_mut().push(Doc::Pdf(pdf));
        self.select_new_page(&page, how);
        self.mark_opened(&page, how);
        self.save_session_soon();
        // The tab is up and says it is opening; the bytes follow, which on a remote vault is a
        // transfer of however long the file takes.
        self.local_copy(key, path, move |app, copy| {
            let Some(pdf) = reader.upgrade() else { return };
            match copy {
                Ok(copy) => pdf.load(&copy),
                Err(e) => app.gone(pdf.key(), &pdf.page, e),
            }
        });
    }

    /// The PDF in the active tab, for the actions that only mean something in one.
    pub fn active_pdf(&self) -> Option<Rc<pdftab::PdfTab>> {
        self.active_doc()?.pdf().cloned()
    }

    /// A PDF that has just been written out goes back to the host, on a remote vault.
    ///
    /// The render thread saves into the file it was handed, which there is the ssh cache copy:
    /// until this runs the document on the host has none of the ink. Blocking ssh I/O, so it is a
    /// worker like every other remote call, and one at a time per tab — the strokes that land
    /// while it is out collapse into one more upload after it.
    fn push_pdf(self: &Rc<Self>, pdf: &Rc<pdftab::PdfTab>) {
        let key = pdf.key();
        let Some(remote) = self
            .vault()
            .filter(|_| !doc::is_loose_key(&key))
            .and_then(|v| v.remote())
            .cloned()
        else {
            return;
        };
        if !pdf.claim_upload() {
            return;
        }
        let (weak_app, weak_pdf) = (Rc::downgrade(self), Rc::downgrade(pdf));
        let edited = pdf.conflict_copy();
        glib::spawn_future_local(async move {
            let asked = key.clone();
            let sent =
                crate::work::off_thread("upload", move || remote.push(&asked, edited.as_deref()))
                    .await;
            let (Some(app), Some(pdf)) = (weak_app.upgrade(), weak_pdf.upgrade()) else {
                return;
            };
            let what = format!("save {}", doc::file_name(&key));
            match sent {
                Some(Ok(accent_api::remote::Pushed::Sent)) => pdf.clear_conflict(),
                // The host's copy moved while this one was being changed. Overwriting it would
                // lose whatever moved it, so the changes went beside it in the vault instead and
                // the reader is told what it is called — once, however long they keep drawing.
                Some(Ok(accent_api::remote::Pushed::Conflict(copy))) => {
                    if pdf.told_conflict(Some(copy.clone())) {
                        app.cannot(
                            &what,
                            format!(
                                "it changed on {}; your changes are saved as {}",
                                app.host(),
                                doc::file_name(&copy)
                            ),
                        );
                    }
                }
                // Not even the copy would go up. The changes are on this machine only, so say
                // where before anything else writes over it.
                Some(Ok(accent_api::remote::Pushed::Kept(at, why))) => {
                    if pdf.told_conflict(None) {
                        app.cannot(
                            &what,
                            format!(
                                "it changed on {} and the copy beside it would not go either \
                                 ({why}); your changes are kept at {}",
                                app.host(),
                                at.display()
                            ),
                        );
                    }
                }
                Some(Err(e)) => app.cannot(&what, e),
                None => app.cannot(&what, "the upload stopped"),
            }
            // Drawn on while it was out, and still the same file: once more, however many saves
            // landed meanwhile.
            if pdf.upload_done() && pdf.key() == key {
                app.push_pdf(&pdf);
            }
        });
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

    /// A blank page to draw on, beside the note that embeds it.
    ///
    /// A one-page PDF and not a format of our own: a sketch is then a document every reader on
    /// the machine can open, and the pen that draws on it is the one that draws on any other PDF.
    pub fn insert_sketch(self: &Rc<Self>) {
        let Some(tab) = self.active() else {
            return self.cannot("add a sketch", "no note is open");
        };
        let rel = tab.rel();
        if doc::is_loose_key(&rel) || self.vault().is_some_and(|v| v.is_remote()) {
            return self.needs_vault("add a sketch");
        }
        let Some(vault) = self.vault() else { return };
        // Beside the note, numbered from one: there is no attachments directory to put it in, and
        // inventing one would be a setting nobody asked for.
        let stem = Path::new(&rel)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy();
        let dir = Path::new(&rel)
            .parent()
            .filter(|p| !p.as_os_str().is_empty());
        let key = (1..)
            .map(|n| match dir {
                Some(dir) => format!("{}/{stem}-sketch-{n}.pdf", dir.display()),
                None => format!("{stem}-sketch-{n}.pdf"),
            })
            .find(|key| !vault.exists(key));
        let Some(key) = key else { return };

        let bytes = match accent_core::pdf::blank_pdf(accent_core::pdf::A4) {
            Ok(bytes) => bytes,
            Err(e) => return self.cannot("make a sketch", e),
        };
        let path = match vault.resolve(&key) {
            Ok(path) => path,
            Err(e) => return self.cannot("make a sketch", e),
        };
        if let Err(e) = accent_core::fs::write_bytes(&path, &bytes, None) {
            return self.cannot(&format!("write {key}"), e);
        }

        tab.buffer.insert_at_cursor(&format!("![[{key}]]"));
        let at = self.pane_of(&tab.page).unwrap_or_else(|| self.pane());
        self.open_beside(&at, Side::Right, &key);
        if let Some(Doc::Pdf(pdf)) = self.doc_for(&key) {
            pdf.set_mode(pdfview::Mode::Pen);
        }
    }

    /// A drawing New Drawing has just written: open it and put the pen down, which is what it
    /// was made for. The tab is the report that it worked.
    ///
    /// The pen is armed through the window action rather than by hand, so a drawing starts in
    /// exactly the state `win.pdf-pen` leaves any other PDF in — the ring out, the tool
    /// remembered, the status bar saying so. The tab has just opened, so it is the active one.
    pub fn open_drawing(self: &Rc<Self>, key: &str) {
        self.open_as(key, Opened::Kept);
        if self.active_pdf().is_some_and(|pdf| pdf.key() == key) {
            self.pdf_mode(pdfview::Mode::Pen);
        }
    }

    /// A blank page before or `after` the one being read in the open PDF.
    ///
    /// Explicit rather than automatic: a stroke cannot reach past the last page to ask for one —
    /// the view clamps a drag to the page under it — so a drawing runs on with Add Page After on
    /// its last page.
    pub fn pdf_add_page(self: &Rc<Self>, after: bool) {
        let Some(pdf) = self.active_pdf() else { return };
        pdf.add_page(after);
    }

    /// Take out the page being read, once the reader has said so. Never the last one.
    pub fn pdf_delete_page(self: &Rc<Self>) {
        let Some(pdf) = self.active_pdf() else { return };
        if pdf.page_count() < 2 {
            return self.cannot("delete the page", "a PDF keeps at least one page");
        }
        pdf.ask_delete_page(pdf.current_page());
    }

    /// Move the page being read one place towards the end (`down`) or the start of the document.
    /// The reader goes with it.
    pub fn pdf_move_page(self: &Rc<Self>, down: bool) {
        let Some(pdf) = self.active_pdf() else { return };
        let from = pdf.current_page();
        let to = match down {
            true => Some(from + 1).filter(|to| *to < pdf.page_count()),
            false => from.checked_sub(1),
        };
        match to {
            Some(to) => pdf.edit_pages(PageEdit::Move { from, to }),
            None if down => self.cannot("move the page down", "it is the last page"),
            None => self.cannot("move the page up", "it is the first page"),
        }
    }

    /// Show or hide the ring of drawing tools over the page.
    ///
    /// The window's state rather than the tab's: the button is in the header, and moving between
    /// two PDFs with the tools out should not put them away.
    pub fn set_drawing(self: &Rc<Self>, showing: bool) {
        // A diagram keeps its own: its ring is out by default, and one diagram's choice must not
        // arm a PDF's pen.
        if let Some(d) = self.active_diagram() {
            d.show_ring(showing, self.ring_at.get());
            self.drawing_button.set_active(showing);
            return self.sync_status();
        }
        let Some(pdf) = self.active_pdf() else { return };
        self.drawing.set(showing);
        self.drawing_button.set_active(showing);
        // Putting the tools away puts the pen down with them; taking them out arms the last tool.
        let tool = match showing {
            true => self.tool.get(),
            false => pdfview::Mode::Select,
        };
        pdf.set_drawing(showing, self.ring_at.get());
        pdf.set_mode(tool);
        self.sync_status();
    }

    /// Pick up one of the tools. The same one twice goes back to reading, the tools staying out.
    pub fn pdf_mode(self: &Rc<Self>, mode: pdfview::Mode) {
        let Some(pdf) = self.active_pdf() else { return };
        let wanted = match pdf.mode() == mode {
            true => pdfview::Mode::Select,
            false => mode,
        };
        // Remembered even when it is put down, so the ring coming back offers the same tool.
        if wanted != pdfview::Mode::Select {
            self.tool.set(wanted);
        }
        // Reaching a tool from the palette with the ring away is what takes it out.
        if wanted != pdfview::Mode::Select && !self.drawing.get() {
            self.drawing.set(true);
            self.drawing_button.set_active(true);
            pdf.set_drawing(true, self.ring_at.get());
        }
        pdf.set_mode(wanted);
        self.sync_status();
    }

    /// A width or a colour picked on the ring: into the config, to every open PDF in every window,
    /// and onto disk a second later (`App::config_changed`).
    fn pdf_choice(&self, tool: pdfview::Mode, choice: pdfview::Choice) {
        choice.apply(tool, &mut self.config.borrow_mut().drawing);
        self.config_changed();
    }

    /// Put the active PDF's tools where this window last had them, and take the position back
    /// from whichever tab is losing them.
    pub fn sync_drawing(&self) {
        if let Some(pdf) = self.active_pdf() {
            // Whatever this tab's ring was dragged to is where the next one starts.
            if let Some(at) = pdf.ring_at() {
                self.ring_at.set(Some(at));
            }
            pdf.set_drawing(self.drawing.get(), self.ring_at.get());
            let showing = self.drawing.get();
            pdf.set_mode(match showing {
                true => self.tool.get(),
                false => pdfview::Mode::Select,
            });
        }
        let diagram = self.active_diagram();
        if let Some(d) = &diagram {
            if let Some(at) = d.ring_at() {
                self.ring_at.set(Some(at));
            }
            d.show_ring(d.ring_shown(), self.ring_at.get());
        }
        self.sync_export();
        // Only a PDF or a diagram can be drawn on, so the button goes with the tab.
        self.drawing_button
            .set_visible(self.active_pdf().is_some() || diagram.is_some());
        self.drawing_button.set_active(match &diagram {
            Some(d) => d.ring_shown(),
            None => self.drawing.get(),
        });
        self.sync_history();
    }

    /// Undo and Redo in the header, while a tool is in hand over the PDF in front — the same
    /// condition `Ctrl+Z` answers under — and either has something to walk. They come and go as a
    /// pair, the one with nothing insensitive: hidden one at a time, Redo appearing beside the
    /// Drawing toggle pushed Undo out from under the pointer.
    ///
    /// Over a diagram every edit is a step, so there the pair shows whenever either side has
    /// something to walk, tool in hand or not.
    pub fn sync_history(&self) {
        let (undo, redo) = match self.active_doc() {
            Some(Doc::Pdf(pdf)) if pdf.mode() != pdfview::Mode::Select => pdf.history(),
            Some(Doc::Diagram(d)) => d.history(),
            _ => (false, false),
        };
        // The same two buttons, named for what they walk.
        let names = match self.active_diagram() {
            Some(_) => ["win.diagram-undo", "win.diagram-redo"],
            None => ["win.pdf-undo", "win.pdf-redo"],
        };
        for (button, action) in [(&self.undo_button, names[0]), (&self.redo_button, names[1])] {
            button.set_tooltip_text(Some(crate::actions::label_of(action)));
        }
        for (button, walks) in [(&self.undo_button, undo), (&self.redo_button, redo)] {
            button.set_visible(undo || redo);
            button.set_sensitive(walks);
        }
    }

    /// Write the note links that highlight the open PDF into the file, as real annotations.
    pub fn export_highlights(self: &Rc<Self>) {
        let Some(pdf) = self.active_pdf() else { return };
        pdf.export_highlights(theme::accent_rgb());
    }

    /// What an export came back with.
    fn exported(self: &Rc<Self>, pdf: &Rc<pdftab::PdfTab>, result: Result<usize, String>) {
        match result {
            Err(e) => self.cannot("export", e),
            Ok(0) => self.toast("Nothing new to export"),
            Ok(n) => {
                let name = doc::file_name(&pdf.key()).to_string();
                let plural = if n == 1 { "highlight" } else { "highlights" };
                self.toast(&format!("Exported {n} {plural} into {name}"));
                // Ask again: the ones now in the file drop out of the painted overlay, because
                // the page's own pixels carry them.
                self.sync_pdf_links(pdf);
            }
        }
    }

    /// Tell a PDF tab which note links highlight it. The index is asked on a worker: this runs
    /// after every save while a PDF is open, and on a remote vault the asking is a round trip.
    fn sync_pdf_links(self: &Rc<Self>, pdf: &Rc<pdftab::PdfTab>) {
        let key = pdf.key();
        // A file outside every vault has no index to ask.
        if doc::is_loose_key(&key) {
            return;
        }
        let Some(vault) = self.vault().cloned() else {
            return;
        };
        let (app, reader) = (Rc::downgrade(self), Rc::downgrade(pdf));
        glib::spawn_future_local(async move {
            let asked = key.clone();
            let links = crate::work::off_thread("pdf links", move || vault.pdf_links(&asked)).await;
            let (Some(app), Some(pdf)) = (app.upgrade(), reader.upgrade()) else {
                return;
            };
            // Renamed while the index was asked: the answer is about a name it no longer has.
            if pdf.key() != key {
                return;
            }
            match links {
                Some(Ok(links)) => pdf.set_note_links(links),
                Some(Err(e)) => tracing::warn!("pdf links for {key}: {e:#}"),
                None => {}
            }
            app.sync_export();
        });
    }

    /// Export Highlights is offered only where there is a highlight to export: over a PDF no note
    /// links to, the command greys out in the page's menu and in the palette rather than being
    /// offered and answering "Nothing new to export".
    pub fn sync_export(&self) {
        let offered = self.active_pdf().is_some_and(|pdf| pdf.has_note_links());
        if let Some(action) = self
            .window
            .lookup_action("pdf-export-highlights")
            .and_downcast::<gio::SimpleAction>()
        {
            action.set_enabled(offered);
        }
    }

    /// The same, once the notes that just changed have reached the index.
    ///
    /// A moment later, and not at once, because the link rows are resolved at the end of the
    /// worker's batch while the event that a file changed is emitted inside it — and our own
    /// saves emit nothing at all, by design.
    ///
    // ponytail: a timer, because there is no event for "the index is current now". The ceiling
    // is a highlight that appears a third of a second after the note is written; an
    // `Event::Indexed` from the worker is the upgrade.
    pub fn sync_pdf_links_soon(self: &Rc<Self>) {
        if self.pdfs().is_empty() {
            return;
        }
        // One timer, restarted: a burst of watcher events is one query per PDF, not one per event.
        self.pdf_links.call(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move || app.sync_all_pdf_links()
        ));
    }

    /// The same for every open PDF, after something changed the notes.
    fn sync_all_pdf_links(self: &Rc<Self>) {
        for pdf in self.pdfs() {
            self.sync_pdf_links(&pdf);
        }
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
        // On the scroller rather than the picture: while the image is fitted it is smaller than
        // the viewport, and a wheel over the empty space around it has to zoom too. Bubble
        // phase, ahead of the scroller's own controller, as everywhere else.
        let viewer = Rc::downgrade(&image);
        zoom_on_wheel(
            &scroller,
            gtk::PropagationPhase::Bubble,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |out, _| {
                    if let Some(image) = viewer.upgrade() {
                        app.zoom_image(&image, Some(out));
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

    /// Show an image tab's file under the look in force, decoded and recoloured off the main
    /// loop: read from `read` when given (opening, a reload), else recoloured again from the
    /// texture it was read to, which a restyle that changes nothing for this image skips.
    ///
    /// Only the latest call's answer lands, and a read that lands under a look that has moved on
    /// since asks again, so a theme change or an inversion while a large file decodes is kept.
    pub(crate) fn show_image(self: &Rc<Self>, image: &Rc<doc::Viewer>, read: Option<PathBuf>) {
        let wanted = (
            look::Look::now(),
            self.inverted_images.borrow().contains(&image.key()),
        );
        let (path, original) = match read {
            // What is on screen is of the old contents, so a restyle meanwhile waits for these.
            Some(path) => {
                image.image.take();
                (path, None)
            }
            None => match image.image.borrow().clone() {
                _ if image.look.get() == Some(wanted) => return,
                Some((path, texture)) => (path, Some(texture)),
                // Still being read, under a look its landing will find is not this one.
                None => return,
            },
        };
        image.look.set(Some(wanted));
        let ticket = image.shows.get().wrapping_add(1);
        image.shows.set(ticket);
        let (app, image) = (Rc::downgrade(self), Rc::downgrade(image));
        glib::spawn_future_local(async move {
            let ((look, inverted), from) = (wanted, path.clone());
            let shown =
                work::off_thread("image", move || look::show(&from, original, look, inverted))
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
                    picture.set_paintable(Some(&shown.texture));
                    // The size a zoomed picture asks for is worked out from its paintable, so a
                    // file of another size would be drawn at its own while the readout kept the
                    // old percentage. Asked again of this one, so the zoom means the same thing
                    // either side of a reload.
                    zoom::set_image_zoom(&picture, image.zoom.get());
                    *image.image.borrow_mut() = Some((path, shown.original));
                    app.show_image(&image, None);
                }
                // A file that never showed goes the way one that never arrived does; one that
                // did keeps its last contents up.
                Some(Err(e)) if picture.paintable().is_none() => {
                    app.gone(image.key(), &image.page, std::io::Error::other(e))
                }
                Some(Err(e)) => app.cannot("reload", e),
                None => app.cannot("reload", "the image worker stopped"),
            }
        });
    }

    /// Invert Image Colours over `key`: recolour it if the theme leaves it alone, show it as it
    /// is if the theme recolours it, in its tabs and in the preview, until the app quits.
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
        self.reshow_preview_images();
    }

    /// Hand `landed` the path on *this* machine holding `key`'s bytes, for the readers that cannot
    /// work with anything else: the PDF engine and an image. That is the file itself on a local
    /// vault and a copy fetched over ssh on a remote one, a transfer of however long the file
    /// takes, so the asking is on a worker; a loose key is already a path here.
    pub(crate) fn local_copy(
        self: &Rc<Self>,
        key: &str,
        path: &Path,
        landed: impl FnOnce(&Rc<App>, std::io::Result<PathBuf>) + 'static,
    ) {
        let vault = self.vault().filter(|_| !doc::is_loose_key(key)).cloned();
        let (key, path) = (key.to_string(), path.to_path_buf());
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let copy = crate::work::off_thread("fetch", move || match vault {
                Some(vault) => vault.fetch(&key),
                None => Ok(path),
            })
            .await
            .unwrap_or_else(|| Err(std::io::Error::other("the fetch stopped")));
            if let Some(app) = weak.upgrade() {
                landed(&app, copy);
            }
        });
    }

    /// A viewer whose bytes never came: its tab goes, the way a note that would not open never
    /// gets one, and the reason is said.
    fn gone(&self, key: String, page: &adw::TabPage, e: std::io::Error) {
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

    /// A comparison of two texts that are not files, as a tab. `key` says which comparison it is,
    /// so asking for the same one twice brings the tab already showing it up to date rather than
    /// stacking a second copy; `file` is the name behind it, which decides how it is coloured.
    /// It opens as the pane's preview, like a file clicked in the tree: a list of changed files
    /// is exactly the surface that would otherwise stack a tab per click.
    pub fn open_diff(
        self: &Rc<Self>,
        key: &str,
        file: &str,
        title: &str,
        old: (&str, &str),
        new: (&str, &str),
    ) -> Rc<diff::DiffTab> {
        if let Some(Doc::Diff(tab)) = self.doc_for(key) {
            tab.set_texts(old.1, new.1);
            self.reveal_page(&tab.page);
            return tab;
        }
        let flavour = match doc::kind_of(file) {
            Kind::Note => Flavour::Note,
            _ => flavour_of(file),
        };
        let font = self.config.borrow().editor_font.clone();
        let tab = diff::DiffTab::open(
            &self.tabs(),
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
        self.select_new_page(&tab.page, Opened::Preview);
        self.mark_opened(&tab.page, Opened::Preview);
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
        self.docs.borrow_mut().push(wrap(viewer.clone()));
        self.select_new_page(&page, how);
        self.mark_opened(&page, how);
        self.save_session_soon();
        viewer
    }

    /// Open a note over the place in it a sidebar row named, so it opens on the match rather than
    /// at the top and the match is revealed where it lands.
    ///
    /// Every row that leads here — a search hit, a tag — is a single click in the sidebar, so
    /// the note opens as a preview and the next such click takes the same tab.
    pub fn open_note_at(self: &Rc<Self>, rel: &str, at: Option<sidebar::Target>) {
        self.mark();
        match at {
            Some(at) => self.select_when_open(rel, at),
            None => self.open_preview(rel),
        }
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

    /// Rewrite every match of `re` in the vault, from the sidebar's Replace All.
    ///
    /// Open tabs are saved first: the vault writes through the etag gate, so an unsaved buffer
    /// would come back as a changed-on-disk banner instead of a replacement. That part is the
    /// main loop's, and so is the reload afterwards; the rewrite between them is not. It is a
    /// read, a substitution and an fsync per file — 1.9 s across 245 notes and 35 s across 3.3k
    /// of them, measured on the generated vault — so it goes to a worker thread and `done` hands
    /// the sidebar back its pane when it lands.
    pub fn replace_in_files(
        self: &Rc<Self>,
        query: String,
        options: accent_api::Options,
        replacement: String,
        literal: bool,
        include_ignored: bool,
        done: Box<dyn FnOnce()>,
    ) {
        let Some(vault) = self.vault().cloned() else {
            done();
            return self.needs_vault("replace across files");
        };
        let Some(ops) = self.ops().cloned() else {
            done();
            return;
        };
        let open: Vec<String> = self.open_tabs().iter().map(|tab| tab.rel()).collect();
        (ops.flush)(&open);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let outcome = crate::work::attempt("replace", move || {
                vault.replace_all(&query, options, &replacement, literal, include_ignored)
            })
            .await;
            if let Some(app) = weak.upgrade() {
                match outcome {
                    Ok(report) => {
                        let unsaved = (ops.reload)(&report.rewritten);
                        let message = replace_message(
                            report.matches,
                            report.rewritten.len(),
                            report.failed.len(),
                            unsaved,
                            report.undoable,
                        );
                        match report.undoable {
                            true => app.toast_with(&message, "Undo", {
                                let weak = Rc::downgrade(&app);
                                move || {
                                    if let Some(app) = weak.upgrade() {
                                        app.undo_replace();
                                    }
                                }
                            }),
                            false => app.toast(&message),
                        }
                    }
                    // Including a worker that stopped: a Replace the user asked for and watched a
                    // progress state run through must never end in silence.
                    Err(why) => app.toast(&why),
                }
            }
            done();
        });
    }

    /// Put back what the last Replace All rewrote: the Undo on the toast it left.
    ///
    /// Open tabs are saved first, as before the rewrite, so a note edited in one since counts as
    /// changed and is left alone rather than written over. The files it did put back are then
    /// reloaded, and the Search pane asks its question again, since its rows are the rewrite's.
    pub fn undo_replace(self: &Rc<Self>) {
        let (Some(vault), Some(ops)) = (self.vault().cloned(), self.ops().cloned()) else {
            return;
        };
        let open: Vec<String> = self.open_tabs().iter().map(|tab| tab.rel()).collect();
        (ops.flush)(&open);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let outcome = crate::work::attempt("undo replace", move || vault.undo_replace()).await;
            let Some(app) = weak.upgrade() else { return };
            match outcome {
                Ok(report) => {
                    (ops.reload)(&report.restored);
                    let message = undo_message(&report);
                    tracing::debug!(toast = message, "replace undone");
                    app.toast(&message);
                    if let Some(sidebar) = app.sidebar.get() {
                        sidebar.requery_search();
                    }
                }
                Err(why) => app.toast(&why),
            }
        });
    }

    /// A link target as written, resolved the way a wikilink resolves: by name, shortest path.
    /// No name is the note on screen, as `[[#Heading]]` writes it. A name nothing in the vault
    /// answers to, in the index or on disk (`Vault::follow`), offers New File with that very path
    /// typed in, which is how a note gets written by being linked to first.
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

    /// Open `rel` and land where `anchor` says: a PDF's page and selection, or a note's heading.
    fn land_on(self: &Rc<Self>, rel: &str, anchor: &str) {
        match accent_core::markdown::pdf_anchor(anchor) {
            Some(at) => {
                self.open_preview(rel);
                self.show_pdf_anchor(rel, Some(at));
            }
            None if anchor.is_empty() => self.open_preview(rel),
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

    /// A link named a heading `tab` does not have: the caret goes to its top, and a toast says
    /// why it is there rather than where the link pointed. Go to Definition and a click in the
    /// preview both end here.
    pub fn no_heading(&self, tab: &Tab, anchor: &str) {
        tab.goto_pos(accent_api::Pos::default());
        let name = doc::file_name(&tab.rel()).to_string();
        self.toast(&format!("No heading {anchor} in {name}"));
    }

    /// Show the page and selection an anchor names, if the tab just opened is that PDF.
    ///
    /// Called straight after the tab is opened rather than through `on_tab`, which waits for a
    /// *text* tab: `open_pdf` has already pushed the document by the time it returns.
    pub fn show_pdf_anchor(&self, key: &str, anchor: Option<PdfAnchor>) {
        let Some((page, selection)) = anchor else {
            return;
        };
        if let Some(Doc::Pdf(pdf)) = self.doc_for(key) {
            pdf.show_link(page, selection);
        }
    }

    /// Where `key` really is: the key to open it under, and the path to read.
    ///
    /// Wikilink targets come out of note content, so `![[../../../../etc/passwd]]` reaches
    /// `open_path` from the preview and has to be stopped here rather than by the reader. The
    /// check is the vault's own lexical one: canonicalising would refuse a note reached through
    /// one of the directory symlinks a vault links in on purpose, which the walk indexed and the
    /// tree is already showing.
    ///
    /// Whether the file is there is not asked here: on a remote vault that is a round trip on the
    /// main thread before every open. The reader's worker finds out anyway, and says so through
    /// [`cannot_open`](Self::cannot_open).
    fn locate(&self, key: &str) -> Option<(String, PathBuf)> {
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
    pub(crate) fn cannot_open(&self, key: &str, e: std::io::Error) {
        match e.kind() {
            std::io::ErrorKind::NotFound => {
                self.cannot(&format!("open {key}"), "not in this vault")
            }
            _ => self.cannot(&format!("open {key}"), e),
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

    /// A tab on a file from outside this window's vault says so on its own tab, so saving it is
    /// never a surprise and it is obvious why it has no backlinks.
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
                move |out, _| app.set_zoom(stepped_zoom(app.zoom.get(), out))
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
                // A code tab's references are about the symbol under the caret, so they follow
                // it — but only while the pane is on screen, since nobody is reading it otherwise.
                if !tab.flavour().is_note()
                    && app.is_active(tab)
                    && app.sidebar_column.is_visible()
                    && app
                        .sidebar
                        .get()
                        .is_some_and(|s| s.is_showing("references"))
                {
                    app.refresh_references();
                }
            }
        ));
        // The chrome hides on the keystroke itself, not on the debounce that follows it.
        tab.buffer.connect_changed(glib::clone!(
            #[weak(rename_to = app)]
            self,
            #[weak]
            tab,
            move |_| app.on_edit(&tab)
        ));

        // A file inside the vault gets a document on the language layer; a loose file has no
        // vault to open it on.
        if let Some(vault) = self
            .vault()
            .filter(|_| !doc::is_loose_key(&tab.rel()))
            .cloned()
        {
            lang::attach(
                &tab,
                vault,
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
                },
            );
        }

        // A pasted or dropped image is written into the vault, which a loose note has none of.
        if tab.flavour().is_note() && self.vault().is_some() && !doc::is_loose_key(&tab.rel()) {
            self.wire_attachments(&tab);
        }

        // Nothing else watches a loose file: the vault's worker only reports on its own tree.
        if doc::is_loose_key(&tab.rel()) {
            tab.watch_file(glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |tab| app.file_changed(tab)
            ));
        }
        self.mark_loose(&tab.page, &tab.rel());
        let page = tab.page.clone();
        self.fetch_head(&tab);
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

    /// The tab view a new tab for `key` goes into: the pane the session restore put it in, while
    /// that pane is still open, and otherwise the active one. A restore is the one open that
    /// knows its pane before the tab exists, and a text tab only exists once the worker's read
    /// lands, so the pane is looked up here rather than the tab moved there afterwards.
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

    /// Put a page that has just been added to a pane in front of it, unless the session restore
    /// opened it: which tab is in front of a restored pane is [`App::put_back`]'s to say.
    ///
    /// Selecting it fires `selected-page`, whose handler runs `sync_active`. The first page in a
    /// pane is selected as it is added, before its document is in `docs`, so that one gets no
    /// notify from here and is synced by hand instead.
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

    /// Run `f` on the tab holding `key`, opening the file first when it has none. An open is a
    /// worker read, so `f` may run later, from [`App::adopt`]; a file that turns out not to be
    /// text never gets there, and its `f` is dropped where that is decided, so it cannot run on
    /// a later open of the same key. `what` is what the reader asked for — "compare", "go to" —
    /// which is what such a drop is said with, so the ask does not simply vanish.
    pub fn with_tab(
        self: &Rc<Self>,
        key: &str,
        how: Opened,
        what: &str,
        f: impl FnOnce(&Rc<App>, &Rc<Tab>) + 'static,
    ) {
        if let Some(tab) = self.tab_for(key) {
            self.reveal_page(&tab.page);
            return f(self, &tab);
        }
        let waiting = Waiting {
            what: (how != Opened::Restored).then(|| format!("{what} {key}")),
            run: Box::new(f),
        };
        self.awaiting.borrow_mut().insert(key.to_string(), waiting);
        self.open_as(key, how);
    }

    /// Drop the work waiting on `key`, saying what it was: the file has turned out to be
    /// something no `Tab` is made for, so a comparison or a jump asked for on it would otherwise
    /// go no further than the tab that did open.
    fn drop_awaiting(&self, key: &str, why: &str) {
        let waiting = self.awaiting.borrow_mut().remove(key);
        if let Some(what) = waiting.and_then(|waiting| waiting.what) {
            self.cannot(&what, why);
        }
    }
}

/// Which editor a text file gets. Only CSV is special: its columns are coloured instead of it
/// being handed to a language, because `csv.lang` would tint numbers and strings underneath.
fn flavour_of(key: &str) -> Flavour {
    match doc::file_name(key).rsplit_once('.') {
        Some((_, ext)) if ext.eq_ignore_ascii_case("csv") => Flavour::Csv,
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

/// What the toast says after a Replace All: what it wrote, what it could not, and what is still
/// showing the old text because its tab has unsaved edits. Same shape as `fileops::rename_message`.
fn replace_message(
    matches: usize,
    files: usize,
    failed: usize,
    unsaved: usize,
    undoable: bool,
) -> String {
    let plural = |n: usize, one: &str, many: &str| match n {
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    };
    let mut message = match matches {
        0 => "Nothing to replace".to_string(),
        _ => format!(
            "Replaced {} in {}",
            plural(matches, "match", "matches"),
            plural(files, "file", "files")
        ),
    };
    if failed > 0 {
        message.push_str(&format!("; {failed} could not be written"));
    }
    if unsaved > 0 {
        message.push_str(&format!(
            "; {unsaved} have unsaved changes and were not reloaded"
        ));
    }
    // Past the size an undo keeps, which the confirmation already warned of.
    if files > 0 && !undoable {
        message.push_str("; it cannot be undone");
    }
    message
}

/// What an Undo on the Replace All toast did. A file changed since the rewrite was left alone,
/// and one such file is named, since the reader may want to go and look at it.
fn undo_message(report: &accent_api::UndoReport) -> String {
    let mut parts = Vec::new();
    match report.restored.len() {
        0 => {}
        1 => parts.push("Restored 1 file".to_string()),
        n => parts.push(format!("Restored {n} files")),
    }
    match &report.skipped[..] {
        [] => {}
        [one] => parts.push(format!("{one} changed since and was left as it is")),
        many => parts.push(format!(
            "{} files changed since and were left as they are",
            many.len()
        )),
    }
    if !report.failed.is_empty() {
        parts.push(format!("{} could not be written", report.failed.len()));
    }
    match parts.is_empty() {
        true => "Nothing to undo".to_string(),
        false => parts.join("; "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_toast_counts_matches_files_and_what_went_wrong() {
        assert_eq!(replace_message(0, 0, 0, 0, false), "Nothing to replace");
        assert_eq!(
            replace_message(1, 1, 0, 0, true),
            "Replaced 1 match in 1 file"
        );
        assert_eq!(
            replace_message(7, 3, 0, 0, true),
            "Replaced 7 matches in 3 files"
        );
        assert_eq!(
            replace_message(7, 3, 1, 2, true),
            "Replaced 7 matches in 3 files; 1 could not be written; 2 have unsaved changes and were not reloaded"
        );
        assert_eq!(
            replace_message(7, 3, 0, 0, false),
            "Replaced 7 matches in 3 files; it cannot be undone"
        );
    }

    #[test]
    fn undo_toast_names_one_skipped_file_and_counts_several() {
        let report = |restored: &[&str], skipped: &[&str]| accent_api::UndoReport {
            restored: restored.iter().map(|s| s.to_string()).collect(),
            skipped: skipped.iter().map(|s| s.to_string()).collect(),
            failed: Vec::new(),
        };
        assert_eq!(undo_message(&report(&[], &[])), "Nothing to undo");
        assert_eq!(undo_message(&report(&["a.md"], &[])), "Restored 1 file");
        assert_eq!(
            undo_message(&report(&["a.md", "b.md"], &["notes/c.md"])),
            "Restored 2 files; notes/c.md changed since and was left as it is"
        );
        assert_eq!(
            undo_message(&report(&[], &["c.md", "d.md"])),
            "2 files changed since and were left as they are"
        );
    }
}
