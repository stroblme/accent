//! The window's side of a PDF tab: opening one, sending its saves to a remote host, the notes'
//! highlights painted on it, and the PDF commands: the pages, the drawing tools, Undo and Redo,
//! Export Highlights and a sketch.

use crate::*;
use accent_core::pdf::PageEdit;

impl App {
    /// A PDF, in the reader.
    pub(crate) fn open_pdf(self: &Rc<Self>, key: &str, path: &Path, how: Opened) {
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
                app.follow_outline();
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
                app.synctex_opened(pdf);
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
            move |pdf| {
                app.synctex_written(pdf);
                app.push_pdf(pdf);
            }
        ));
        pdf.connect_export(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |pdf, result| app.exported(pdf, result)
        ));
        // The notes name pages by number, and so do the pane's places: both follow a page edit.
        pdf.connect_repaged(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |pdf, edit, step| {
                app.synctex_repaged(pdf);
                app.repaged(pdf, edit, step);
            }
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
        *pdf.monitor.borrow_mut() = self.watch_loose(key, path);
        let reader = Rc::downgrade(&pdf);
        self.watch_folder_of(key);
        self.docs.borrow_mut().push(Doc::Pdf(pdf));
        self.select_new_page(&page, how);
        self.mark_opened(&page, how);
        self.save_session_soon();
        // The tab is up and says it is opening; the bytes follow, which on a remote vault is a
        // transfer of however long the file takes.
        self.local_copy(key, path, move |app, copy| {
            let Some(pdf) = reader.upgrade() else { return };
            match copy {
                Ok(copy) => {
                    pdf.load(&copy);
                    app.push_pdf(&pdf);
                }
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
    /// while it is out collapse into one more upload after it. A copy with nothing unsent sends
    /// nothing, so a tab just opened asks too: an earlier one, or the last session, may have left
    /// strokes the host never took. The tab is held until the answer, which a tab closed after its
    /// last stroke still has to hear.
    pub(crate) fn push_pdf(self: &Rc<Self>, pdf: &Rc<pdftab::PdfTab>) {
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
        let (weak_app, pdf) = (Rc::downgrade(self), pdf.clone());
        glib::spawn_future_local(async move {
            let asked = key.clone();
            let sent = crate::work::off_thread("upload", move || remote.push(&asked)).await;
            let Some(app) = weak_app.upgrade() else {
                return;
            };
            let what = format!("save {}", doc::file_name(&key));
            // Renamed while it was out: the tab has followed, and what did not reach the host goes
            // again under the new name, below.
            let renamed = pdf.key() != key;
            match sent {
                Some(Ok(accent_api::remote::Pushed::Sent)) => pdf.clear_conflict(),
                // The host's copy moved while this one was being changed. Overwriting it would
                // lose whatever moved it, so the changes went beside it in the vault instead, and
                // the tab follows them there: what is drawn next goes into the copy it shows.
                Some(Ok(accent_api::remote::Pushed::Conflict(copy))) if !renamed => {
                    app.cannot(
                        &what,
                        format!(
                            "it changed on {}; your changes are saved as {}, which this tab now \
                             shows",
                            app.host(),
                            doc::file_name(&copy)
                        ),
                    );
                    app.move_onto_copy(&pdf, &copy);
                }
                // Renamed while it was out, so the tab is on another file: only say where.
                Some(Ok(accent_api::remote::Pushed::Conflict(copy))) => app.cannot(
                    &what,
                    format!(
                        "it changed on {}; your changes are saved as {}",
                        app.host(),
                        doc::file_name(&copy)
                    ),
                ),
                // Not even the copy would go up. The changes are on this machine only, so say
                // where before anything else writes over it.
                Some(Ok(accent_api::remote::Pushed::Kept(at, why))) => {
                    pdf.lost_upload();
                    if pdf.told_conflict() {
                        app.retry_upload(
                            &pdf,
                            &format!(
                                "Cannot {what}: it changed on {} and the copy beside it would \
                                 not go either ({why}); your changes are kept at {}",
                                app.host(),
                                at.display()
                            ),
                        );
                    }
                }
                // The file was renamed while this was on its way, and it goes again below; or the
                // link went, which the banner says as it does for a note that cannot save, and it
                // goes again on `Event::Connected`.
                Some(Err(e)) if renamed || e.kind() == std::io::ErrorKind::NotConnected => {
                    pdf.lost_upload()
                }
                // It did not reach the host at all. Said once, as a refusal is: every stroke after
                // it fails the same way until one lands, or Retry is pressed.
                Some(Err(e)) => {
                    if pdf.told_failure() {
                        app.retry_upload(&pdf, &format!("Cannot {what}: {e}"));
                    }
                }
                None => {
                    if pdf.told_failure() {
                        app.retry_upload(&pdf, &format!("Cannot {what}: the upload stopped"));
                    }
                }
            }
            // Drawn on while it was out: once more, however many saves landed meanwhile.
            if pdf.upload_done() || (renamed && pdf.unsent()) {
                app.push_pdf(&pdf);
            }
        });
    }

    /// Point the tab of `pdf` at `copy`, where its changes went up and its cached copy has gone
    /// (`Pushed::Conflict`): as a rename moves a tab, and this tab alone, the original staying
    /// in the vault, the recent files and any other window as it was.
    fn move_onto_copy(self: &Rc<Self>, pdf: &pdftab::PdfTab, copy: &str) {
        pdf.retarget(&self.root(), copy);
        pdf.clear_conflict();
        self.sync_active();
        self.save_session_soon();
    }

    /// Say that a PDF's changes did not reach the host, with a Retry: nothing tells the window
    /// that the host's folder takes writes again, and a reader who has stopped drawing would
    /// otherwise have nothing to send them with. A failure after the Retry is said again. The
    /// toast holds the tab, which may be closing with the last strokes.
    fn retry_upload(self: &Rc<Self>, pdf: &Rc<pdftab::PdfTab>, message: &str) {
        let (app, pdf) = (Rc::downgrade(self), pdf.clone());
        self.toast_with(message, "Retry", move || {
            if let Some(app) = app.upgrade() {
                pdf.forget_told();
                app.push_pdf(&pdf);
            }
        });
    }

    /// A blank page to draw on, beside the note that embeds it.
    ///
    /// A one-page PDF and not a format of our own: a sketch is then a document every reader on
    /// the machine can open, and the pen that draws on it is the one that draws on any other PDF.
    /// Written through the vault, so on a remote one it lands on the host.
    pub fn insert_sketch(self: &Rc<Self>) {
        let Some(tab) = self.active() else {
            return self.cannot("add a sketch", "no note is open");
        };
        let rel = tab.rel();
        if doc::is_loose_key(&rel) {
            return self.needs_vault("add a sketch");
        }
        let Some(vault) = self.vault().cloned() else {
            return;
        };
        // Beside the note, numbered from one: there is no attachments directory to put it in, and
        // inventing one would be a setting nobody asked for.
        let stem = Path::new(&rel)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let dir = accent_core::path::parent_dir(&rel).to_string();
        let (app, tab) = (Rc::downgrade(self), Rc::downgrade(&tab));
        glib::spawn_future_local(async move {
            // A round trip per name tried on a remote vault, and pdfium work behind the
            // process-wide lock a render thread may be holding: a worker's, as New Drawing's is.
            let made = crate::work::off_thread("sketch", move || {
                let mut n = 1;
                let key = loop {
                    let key = match dir.is_empty() {
                        true => format!("{stem}-sketch-{n}.pdf"),
                        false => format!("{dir}/{stem}-sketch-{n}.pdf"),
                    };
                    // A name the vault cannot answer for is not taken as free: the write would
                    // replace whatever is there.
                    match vault.stat(&key) {
                        Ok(None) => break key,
                        Ok(Some(_)) => n += 1,
                        Err(e) => return Err(("make a sketch".to_string(), e.to_string())),
                    }
                };
                let bytes = accent_core::pdf::blank_pdf(accent_core::pdf::A4)
                    .map_err(|e| ("make a sketch".to_string(), format!("{e:#}")))?;
                vault
                    .write_file(&key, &bytes)
                    .map_err(|e| (format!("write {key}"), e.to_string()))?;
                Ok(key)
            })
            .await
            .unwrap_or_else(|| Err(("make a sketch".to_string(), "it stopped".to_string())));
            let (Some(app), Some(tab)) = (app.upgrade(), tab.upgrade()) else {
                return;
            };
            let key = match made {
                Ok(key) => key,
                Err((what, why)) => return app.cannot(&what, why),
            };
            tab.buffer.insert_at_cursor(&format!("![[{key}]]"));
            let at = app.pane_of(&tab.page).unwrap_or_else(|| app.pane());
            app.open_beside(&at, Side::Right, &key);
            if let Some(Doc::Pdf(pdf)) = app.doc_for(&key) {
                pdf.set_mode(pdfview::Mode::Pen);
            }
        });
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

    /// A blank page before or `after` the one the page's menu was opened on in the open PDF, else
    /// the one being read.
    ///
    /// Explicit rather than automatic: a stroke cannot reach past the last page to ask for one —
    /// the view clamps a drag to the page under it — so a drawing runs on with Add Page After on
    /// its last page.
    pub fn pdf_add_page(self: &Rc<Self>, after: bool) {
        let Some(pdf) = self.active_pdf() else { return };
        pdf.add_page(after);
    }

    /// Take out the page the page's menu was opened on, else the page being read, at once: Undo
    /// puts it back. Never the last one.
    pub fn pdf_delete_page(self: &Rc<Self>) {
        let Some(pdf) = self.active_pdf() else { return };
        let page = pdf.command_page();
        if pdf.page_count() < 2 {
            return self.cannot("delete the page", "a PDF keeps at least one page");
        }
        pdf.edit_pages(PageEdit::Delete(page));
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
            // Presenting puts the tools away and the tool in hand down, so nothing draws on what
            // is presented; the window still knows they were out.
            let showing = self.drawing.get() && self.presenting.get().is_none();
            pdf.set_drawing(showing, self.ring_at.get());
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
        self.sync_diagram_tools();
        self.sync_history();
    }

    /// Undo and Redo in the header, while the PDF or the diagram in front has something for
    /// either to walk, tool in hand or not: a page edit is a step as a stroke is. They come and go
    /// as a pair, the one with nothing insensitive: hidden one at a time, Redo appearing beside
    /// the Drawing toggle pushed Undo out from under the pointer.
    pub fn sync_history(&self) {
        let (undo, redo) = match self.active_doc() {
            Some(Doc::Pdf(pdf)) => pdf.history(),
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
}
