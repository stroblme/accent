//! Saving a tab and what happens when the disk disagrees: the etag check, the overwrite and
//! unsaved-changes questions, and resolving a sync conflict.

use super::*;
use crate::editor::Saves;

impl App {
    pub fn save_active(self: &Rc<Self>) {
        if let Some(tab) = self.active() {
            self.save_tab(&tab, true);
        } else if let Some(diagram) = self.active_diagram() {
            self.save_diagram(&diagram, true);
        }
    }

    /// `explicit` is a Ctrl+S, which may raise a dialog. An autosave never can: interrupting
    /// someone mid-sentence with a modal is exactly what autosave exists to avoid.
    ///
    /// A buffer whose file moved underneath it is not written at all, and the save is not even
    /// attempted ([`editor::may_save`]): the banner is holding a question and a save is not an
    /// answer to it. Ctrl+S raises the same dialog a refused write raises, so a reflex save is
    /// never silently dropped; an autosave says nothing beyond the banner already on screen.
    ///
    /// The two paths a tab leaves by — a tab closing, a window closing — deliberately do *not*
    /// come through here. They write and then ask when the write is refused, because a buffer on
    /// its way out has nowhere else to be kept and refusing there would lose it outright.
    pub fn save_tab(self: &Rc<Self>, tab: &Rc<Tab>, explicit: bool) {
        if !editor::may_save(tab.save.modified.get(), tab.save.disk_changed.get()) {
            if explicit {
                // A note deleted underneath us is asking to be written back, and its banner's
                // button already says Save, so Ctrl+S does that rather than offering to
                // overwrite a file that is not there.
                match tab.alert() {
                    Some(Alert::Restore) => self.answer_banner(tab),
                    _ => self.ask_overwrite(tab),
                }
            }
            return;
        }
        self.start_save(tab, explicit, Self::save_tab, Self::land_save);
    }

    /// The save a note and a diagram share, once the tab's own save has let it through: `again`
    /// is that save, for one asked for meanwhile, and `land` is how the tab takes the answer in.
    ///
    /// The write runs on a worker and lands later ([`Self::land_flight`]): every window shares the
    /// one main thread, and on a remote vault a save is a round trip that held all of them, once
    /// a second while anyone typed. One save per tab is on its way at a time; one asked for
    /// meanwhile runs when it lands, gated on the etag it brought back.
    pub(crate) fn start_save<T: Saves>(
        self: &Rc<Self>,
        tab: &Rc<T>,
        explicit: bool,
        again: fn(&Rc<Self>, &Rc<T>, bool),
        land: fn(&Rc<Self>, &Rc<T>, bool) -> Option<bool>,
    ) {
        let save = tab.save_state();
        if save.flight.borrow().is_some() {
            let asked = save.save_again.get().unwrap_or(false);
            save.save_again.set(Some(asked || explicit));
            return;
        }
        let (write, text, expected) = (self.writer(tab), tab.for_disk(), save.etag.get());
        let (tx, answer) = std::sync::mpsc::channel();
        *save.flight.borrow_mut() = Some(editor::Flight {
            started: save.edits.get(),
            expected,
            explicit,
            answer,
        });
        // Started here and not inside the future below, which runs a turn of the loop later: a
        // write that has to wait for this one may be asked for in this same turn, and would wait
        // on a worker that had not begun. The answer goes through the channel; the worker's own
        // end is only the news that it is there.
        let worker = gio::spawn_blocking(move || tx.send(write(&text, expected)));
        let (app, saved) = (Rc::downgrade(self), Rc::downgrade(tab));
        glib::spawn_future_local(async move {
            let _ = worker.await;
            let (Some(app), Some(tab)) = (app.upgrade(), saved.upgrade()) else {
                return;
            };
            // One asked for meanwhile, unless this one failed: that would meet the same refusal.
            let asked = tab.save_state().save_again.take();
            match (land(&app, &tab, true), asked) {
                (Some(true), Some(explicit)) if tab.save_state().modified.get() => {
                    again(&app, &tab, explicit);
                }
                (Some(true), Some(true)) => app.toast("Saved"),
                _ => {}
            }
        });
    }

    /// [`Self::land_flight`] for a note: `report` is false for a write about to go after it.
    fn land_save(self: &Rc<Self>, tab: &Rc<Tab>, report: bool) -> Option<bool> {
        self.land_flight(
            tab,
            Self::file_changed,
            |app, tab, landed, explicit| match landed {
                editor::Landing::Clean(etag) | editor::Landing::Behind(etag) => {
                    app.wrote(tab, etag, matches!(landed, editor::Landing::Clean(_)));
                    app.sync_pdf_links_soon();
                    // A note carries its tags, and the text a search matched, in its own body —
                    // and our own save is the one write the vault reports nothing about. Without
                    // this both panes would only ever follow what another editor did, which is
                    // also what leaves the find bar's Replace All inside this very tab unseen.
                    if let Some(sidebar) = app.sidebar.get() {
                        sidebar.mark_tags_dirty();
                        sidebar.requery_search_soon();
                    }
                    if report && explicit {
                        app.toast("Saved");
                    }
                }
                editor::Landing::Failed(e) if report => app.refused(tab, e, explicit),
                editor::Landing::Failed(_) | editor::Landing::Stale => {}
            },
        )
    }

    /// Take in the save on its way for `tab`, waiting for its write if that has not finished:
    /// `None` when there was none, else whether it did not fail. Whoever comes first applies it:
    /// the save's own landing, or a write that has to go after it, which does not report it
    /// because it is about to meet whatever refused this one and will say so itself.
    ///
    /// `apply` is what the landing means to the tab's kind, told whether a Ctrl+S asked for it;
    /// `recheck` is the kind's watcher handler, for a stat that waited on the save.
    pub(crate) fn land_flight<T: Saves>(
        self: &Rc<Self>,
        tab: &Rc<T>,
        recheck: fn(&Rc<Self>, &Rc<T>),
        apply: impl FnOnce(&Rc<Self>, &Rc<T>, editor::Landing, bool),
    ) -> Option<bool> {
        let save = tab.save_state();
        let flight = save.flight.take()?;
        let written = flight
            .answer
            .recv()
            .unwrap_or_else(|_| Err(std::io::Error::other("the save worker panicked").into()));
        log_write(&tab.key(), flight.expected, &written);
        let landed = editor::landing(
            flight.started,
            save.edits.get(),
            flight.expected,
            save.etag.get(),
            written,
        );
        let landed_ok = !matches!(landed, editor::Landing::Failed(_));
        apply(self, tab, landed, flight.explicit);
        if save.recheck.take() {
            recheck(self, tab);
        }
        Some(landed_ok)
    }

    /// A save that did not land, said the way its kind needs saying.
    fn refused(self: &Rc<Self>, tab: &Rc<Tab>, e: SaveError, explicit: bool) {
        match e {
            // The etag gate refused: the tab holds the question from now on, whatever asked. It
            // used to be recorded only for an autosave, so a Ctrl+S that was cancelled left a
            // blocked tab with no banner on it.
            SaveError::ChangedOnDisk { .. } => {
                tab.save.disk_changed.set(true);
                tab.show_alert(Alert::Compare);
                if explicit {
                    self.ask_overwrite(tab);
                }
            }
            // Offline is not a disk error: nothing is wrong with the file, the vault is simply
            // not there to write to. The banner across the window is the one place that says so,
            // and it has the way back, so an autosave that cannot land every few seconds says
            // nothing further (DESIGN.md, States: a state that persists is a banner, and a toast
            // is a thing that happened and is over).
            SaveError::Offline => {
                self.show_offline();
                if explicit {
                    self.toast("Not connected, so nothing was saved");
                }
            }
            e => self.cannot("save", e),
        }
    }

    /// Say the vault is unreachable, unless something already is.
    ///
    /// A save can notice the link is down before the connection thread has reported it, and the
    /// banner it would raise then must not overwrite the reason a real `Event::Disconnected` gave.
    pub(crate) fn show_offline(&self) {
        if !self.connection.is_revealed() {
            self.show_connection_banner("The vault is not answering");
        }
    }

    /// The write itself, as something a worker can run. What the file should hold, not what the
    /// buffer holds, is the caller's: a code file loses its trailing whitespace and a DOS file
    /// gets its CRLFs back in [`Tab::for_disk`].
    ///
    /// A loose tab is not in any vault, so it writes through core directly. Same atomic save,
    /// same etag gate; what it misses is the watcher being told the write was ours, which the
    /// tab's own file monitor makes harmless.
    pub(crate) fn writer<T: Saves>(
        &self,
        tab: &Rc<T>,
    ) -> impl FnOnce(&str, Option<Etag>) -> Result<Etag, SaveError> + Send + 'static {
        let (rel, path) = (tab.key(), tab.path());
        let vault = self.vault().filter(|_| !doc::is_loose_key(&rel)).cloned();
        move |text, expected| match vault {
            Some(vault) => vault.save(&rel, text, expected),
            None => accent_core::fs::write_note(&path, text, expected),
        }
    }

    /// What follows a write that landed: the etag it left, and — when the buffer is still what
    /// was written — the tab clean again.
    fn wrote(&self, tab: &Rc<Tab>, etag: Etag, clean: bool) {
        if clean {
            tab.mark_clean(etag);
            tab.clear_disk_alert();
            // The tab is clean again, so the bar's dot goes with the one on the tab title.
            self.sync_status();
        } else {
            tab.save.etag.set(Some(etag));
        }
        lang::saved(tab);
        // Our own writes go through the vault, which tells the watcher they were ours, so no
        // event comes back to say the working tree moved. The pane is told here instead.
        if let Some(git) = self.git.get() {
            git.schedule_refresh(git::Depth::Status);
        }
    }

    /// Write the buffer before returning, and hand the error back instead of reporting it: a
    /// caller that is about to make the buffer unreachable has to know whether the bytes landed.
    /// A save still on its way lands first, so the two never race for the file.
    pub fn write_tab(
        self: &Rc<Self>,
        tab: &Rc<Tab>,
        expected: Option<Etag>,
    ) -> Result<(), SaveError> {
        self.land_save(tab, false);
        let written = self.writer(tab)(&tab.for_disk(), expected);
        log_write(&tab.rel(), expected, &written);
        self.wrote(tab, written?, true);
        Ok(())
    }

    /// [`save_tab`](Self::save_tab) written before it returns, for a buffer whose file is about
    /// to move: an autosave in every other respect.
    pub fn save_tab_now(self: &Rc<Self>, tab: &Rc<Tab>) {
        if !editor::may_save(tab.save.modified.get(), tab.save.disk_changed.get()) {
            return;
        }
        if let Err(e) = self.flush_tab(tab) {
            self.refused(tab, e, false);
        }
    }

    /// [`write_tab`](Self::write_tab) gated on the tab's own etag, as it stands once any save on
    /// its way has landed; a buffer that landing left clean has nothing more to write. For the
    /// paths a buffer leaves by, and for a file about to move with its buffer dirty.
    pub fn flush_tab(self: &Rc<Self>, tab: &Rc<Tab>) -> Result<(), SaveError> {
        self.land_save(tab, false);
        if !tab.save.modified.get() {
            return Ok(());
        }
        self.write_tab(tab, tab.save.etag.get())
    }

    /// A watcher says the file under a note moved ([`Self::check_disk`]).
    ///
    /// Only for a watcher. Every other caller of [`Self::refresh_tab`] is answering a question
    /// the user was asked, and has to reload whatever the etag says.
    pub fn file_changed(self: &Rc<Self>, tab: &Rc<Tab>) {
        self.check_disk(tab, |app, tab| {
            app.refresh_tab(tab);
        });
    }

    /// A watcher says the file under a tab moved: `moved` runs when what is there is not what
    /// the tab last read or wrote.
    ///
    /// Whose write it was is the first question. Every save is a rename into place, which a file
    /// monitor reports as a change like anyone else's, so the etag is the only thing that tells
    /// our own writes apart from a real one: a file still carrying the etag we wrote holds
    /// exactly what the buffer already has. Reloading it anyway threw the view at the caret a
    /// second after every keystroke, and on a buffer typed into since the save it raised a
    /// "changed on disk" banner against our own bytes.
    ///
    /// A stat that failed is not an answer and must not read as one. It used to fall in with "no
    /// file there", which differs from any etag we hold and so raised the banner: on a remote
    /// vault a dropped ssh connection would report a conflict over a diff holding nothing but the
    /// user's own edits. Nothing is lost by waiting — a real change fires the watcher again, and
    /// the etag gate refuses any save that would clobber one in the meantime.
    ///
    /// The stat runs on a worker, being a round trip on a remote vault.
    pub(crate) fn check_disk<T: Saves>(self: &Rc<Self>, tab: &Rc<T>, moved: fn(&Rc<Self>, &Rc<T>)) {
        let (rel, path) = (tab.key(), tab.path());
        let vault = self.vault().filter(|_| !doc::is_loose_key(&rel)).cloned();
        let (app, watched) = (Rc::downgrade(self), Rc::downgrade(tab));
        glib::spawn_future_local(async move {
            let looked = crate::work::off_thread("stat", move || match vault {
                Some(vault) => vault.stat(&rel),
                None => match Etag::of(&path) {
                    Ok(etag) => Ok(Some(etag)),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(e),
                },
            })
            .await
            .unwrap_or_else(|| Err(std::io::Error::other("the stat worker stopped")));
            if let (Some(app), Some(tab)) = (app.upgrade(), watched.upgrade()) {
                app.compare_disk(&tab, looked, moved);
            }
        });
    }

    /// What [`check_disk`](Self::check_disk) does once the stat is in.
    fn compare_disk<T: Saves>(
        self: &Rc<Self>,
        tab: &Rc<T>,
        looked: std::io::Result<Option<Etag>>,
        moved: fn(&Rc<Self>, &Rc<T>),
    ) {
        let save = tab.save_state();
        // A save still on its way has not put its etag in the tab yet, so our own write would
        // read as someone else's. Looked at again once it has landed.
        if save.flight.borrow().is_some() {
            save.recheck.set(true);
            return;
        }
        let disk = match looked {
            Ok(disk) => disk,
            Err(e) => {
                return tracing::debug!(
                    target: SAVES, rel = %tab.key(), error = %e, "watcher: could not stat"
                );
            }
        };
        let ours = save.etag.get();
        tracing::debug!(
            target: SAVES,
            rel = %tab.key(),
            ?ours,
            ?disk,
            modified = save.modified.get(),
            "watcher"
        );
        if ours != disk {
            moved(self, tab);
        }
    }

    /// Refresh a tab from what is on disk, unless its buffer holds edits nobody has saved: that
    /// buffer is the only copy of them, so the banner asks instead of the reload deciding.
    /// Returns whether the tab was refreshed.
    pub fn refresh_tab(self: &Rc<Self>, tab: &Rc<Tab>) -> bool {
        if tab.save.modified.get() {
            tab.save.disk_changed.set(true);
            tab.show_alert(Alert::Compare);
            return false;
        }
        let vault = self
            .vault()
            .filter(|_| !doc::is_loose_key(&tab.rel()))
            .cloned();
        // The read is off the main thread, so the status is asked for when the text lands rather
        // than here: a reload writes the buffer without an edit event, and nothing else would ask.
        tab.reload_keep_cursor(
            vault,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |_: &Rc<Tab>, read: std::io::Result<()>| {
                    if let Err(e) = read {
                        app.cannot("reload", e);
                    }
                    app.sync_status();
                }
            ),
        );
        true
    }

    fn ask_overwrite(self: &Rc<Self>, tab: &Rc<Tab>) {
        let dialog = adw::AlertDialog::new(
            Some("File Changed on Disk"),
            Some(&format!(
                "{} was modified elsewhere since you opened it.",
                tab.rel()
            )),
        );
        // Compare rather than Reload: reloading threw the buffer away on one click, and the
        // resolver shows both sides and now lets them be merged by hand.
        dialog.add_responses(&[
            ("cancel", "Cancel"),
            ("compare", "Compare"),
            ("overwrite", "Overwrite"),
        ]);
        dialog.set_response_appearance("overwrite", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        let (app, tab) = (self.clone(), tab.clone());
        dialogs::choose(
            &dialog,
            Some(&self.window),
            move |response| match response.as_str() {
                "compare" => app.compare_with_disk(&tab),
                "overwrite" => match app.write_tab(&tab, None) {
                    Ok(()) => app.toast("Overwritten"),
                    Err(e) => app.cannot("save", e),
                },
                _ => {}
            },
        );
    }

    /// A tab is on its way out and its buffer could not be written. Ask, then call `after` with
    /// whether the tab may go: the answer decides, never the failed save.
    ///
    /// DESIGN.md, States: the choice can lose data either way round, so it is an `AlertDialog`
    /// naming both losses rather than a toast behind a window that is already closing.
    pub fn ask_unsaved(
        self: &Rc<Self>,
        tab: &Rc<Tab>,
        error: &SaveError,
        after: impl Fn(&Rc<Self>, bool) + 'static,
    ) {
        let (discarded, written) = (tab.clone(), tab.clone());
        self.ask_unsaved_about(
            &tab.rel(),
            error,
            move || discarded.discard(),
            move |app| app.write_tab(&written, None),
            after,
        );
    }

    /// The same question for a diagram on its way out.
    pub(crate) fn ask_unsaved_diagram(
        self: &Rc<Self>,
        tab: &Rc<crate::diagram::DiagramTab>,
        error: &SaveError,
        after: impl Fn(&Rc<Self>, bool) + 'static,
    ) {
        let (discarded, written) = (tab.clone(), tab.clone());
        self.ask_unsaved_about(
            &tab.key(),
            error,
            move || discarded.discard(),
            move |app| app.write_diagram(&written, None),
            after,
        );
    }

    /// The question itself, whatever kind of document holds the edits: `discard` throws them
    /// away, `overwrite` writes them over whatever is on disk.
    fn ask_unsaved_about(
        self: &Rc<Self>,
        rel: &str,
        error: &SaveError,
        discard: impl FnOnce() + 'static,
        overwrite: impl FnOnce(&Rc<Self>) -> Result<(), SaveError> + 'static,
        after: impl Fn(&Rc<Self>, bool) + 'static,
    ) {
        let body = match error {
            SaveError::ChangedOnDisk { .. } => {
                format!("{rel} changed on disk, so your edits could not be saved.")
            }
            e => format!("{rel} could not be saved: {e}"),
        };
        let dialog = adw::AlertDialog::new(Some("Unsaved Changes"), Some(&body));
        // Overwriting means writing again, which is the very thing that just failed for want of a
        // connection: offering it would be offering nothing.
        let mut responses: Vec<(&str, &str)> = vec![("cancel", "Cancel"), ("discard", "Discard")];
        if !matches!(error, SaveError::Offline) {
            responses.push(("overwrite", "Overwrite"));
        }
        dialog.add_responses(&responses);
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        let app = self.clone();
        dialogs::choose(&dialog, Some(&self.window), move |response| {
            let close = match response.as_str() {
                "discard" => {
                    discard();
                    true
                }
                "overwrite" => match overwrite(&app) {
                    Ok(()) => true,
                    Err(e) => {
                        app.cannot("save", e);
                        false
                    }
                },
                _ => false,
            };
            after(&app, close);
        });
    }

    /// Forget a page that is really closing. Called on every path that closes one, because
    /// `close_page_finish` does not come back through the `close-page` handler.
    pub fn forget_page(self: &Rc<Self>, page: &adw::TabPage) {
        // A drawn-on document leaving: the render thread drains its channel before it ends, so
        // the write still happens after the tab is gone.
        if let Some(Doc::Pdf(pdf)) = self.doc_for_page(page) {
            pdf.flush();
            if let Some(at) = pdf.ring_at() {
                self.ring_at.set(Some(at));
            }
        }
        // A diagram closing was asked about already (`wire.rs`), so a failure here is the
        // answer "discard" having been given, or the file gone. A label still being typed goes
        // into its cell first: it is an edit like any other, and its editor holds a display-wide
        // font that only closing the editor lets go of.
        if let Some(Doc::Diagram(d)) = self.doc_for_page(page) {
            d.finish_label();
            if let Err(e) = self.flush_diagram(&d) {
                tracing::debug!("a closing diagram was not written: {e}");
            }
        }
        // Closing a shell's tab is ending it; closing its window only lets go of it, and a window
        // being destroyed closes no page through here. A shell is refused a move to another
        // window before it would reach here too.
        if let Some(Doc::Terminal(term)) = self.doc_for_page(page) {
            term.kill();
        }
        if let Some((pane, doc)) = self.pane_of(page).zip(self.doc_for_page(page)) {
            pane.nav.borrow_mut().forget(&doc.key());
        }
        self.docs.borrow_mut().retain(|d| d.page() != page);
        self.pinned.borrow_mut().retain(|p| p != page);
        self.sync_active();
        self.save_session_soon();
    }

    /// The banner's button, doing what its label says. Which is which is decided when the banner
    /// goes up, not read off the file system when the button is pressed.
    pub fn answer_banner(self: &Rc<Self>, tab: &Rc<Tab>) {
        match tab.alert() {
            // Reports rather than asks, so it has no button and this cannot be reached from one.
            Some(Alert::ReadOnly) => {}
            // Both sides hold work, so neither is thrown away on one click: the diff shows what
            // differs and the user picks (DESIGN.md: a choice that can lose data is a dialog).
            Some(Alert::Compare) => self.compare_with_disk(tab),
            Some(Alert::Restore) => match self.write_tab(tab, None) {
                Ok(()) => self.toast("Saved"),
                Err(e) => self.cannot("save", e),
            },
            // Looked up again rather than remembered: the copy may have been resolved from
            // another window, or by Syncthing, since the banner went up. On a worker, being a
            // round trip on a remote vault.
            Some(Alert::Conflict) => {
                let Some(vault) = self.vault().cloned() else {
                    return;
                };
                let rel = tab.rel();
                let (app, asked) = (Rc::downgrade(self), Rc::downgrade(tab));
                glib::spawn_future_local(async move {
                    let copies = crate::work::off_thread("conflict list", {
                        let rel = rel.clone();
                        move || vault.conflicts_of(&rel)
                    })
                    .await;
                    let (Some(app), Some(tab)) = (app.upgrade(), asked.upgrade()) else {
                        return;
                    };
                    match copies.and_then(Result::ok).unwrap_or_default().first() {
                        Some(conflict) => app.resolve_conflict(&rel, conflict),
                        None => {
                            tab.clear_alert(Alert::Conflict);
                            app.toast("The conflict copy is gone");
                        }
                    }
                });
            }
            None => tab.hide_banner(),
        }
    }

    /// The unsaved buffer against the file underneath it, in the tab itself: the editor is the
    /// Mine pane, so a merge is typed straight into the note.
    ///
    /// The file is read on a worker, being a round trip on a remote vault.
    pub fn compare_with_disk(self: &Rc<Self>, tab: &Rc<Tab>) {
        let vault = self
            .vault()
            .filter(|_| !doc::is_loose_key(&tab.rel()))
            .cloned();
        let (rel, path) = (tab.rel(), tab.path());
        let (app, asked) = (Rc::downgrade(self), Rc::downgrade(tab));
        glib::spawn_future_local(async move {
            let read = crate::work::off_thread("reader", move || match vault {
                Some(vault) => vault.read(&rel),
                None => accent_core::fs::read_note(&path),
            })
            .await;
            if let (Some(app), Some(tab)) = (app.upgrade(), asked.upgrade()) {
                app.compare_with(&tab, read.and_then(Result::ok));
            }
        });
    }

    /// What [`compare_with_disk`](Self::compare_with_disk) does once the file is read.
    fn compare_with(self: &Rc<Self>, tab: &Rc<Tab>, read: Option<(String, Etag)>) {
        let rel = tab.rel();
        let Some((disk, disk_etag)) = read else {
            return self.toast(&format!("Cannot read {rel} from disk"));
        };
        let keep_theirs = gtk::Button::with_label("Keep Theirs");
        let keep_mine = gtk::Button::with_label("Keep Mine");
        keep_mine.add_css_class("suggested-action");
        // Keeping theirs drops the buffer, which is a loss the user has now seen spelled out
        // line by line.
        keep_theirs.connect_clicked(glib::clone!(
            #[weak(rename_to = app)]
            self,
            #[weak]
            tab,
            move |_| {
                tab.leave_compare();
                tab.discard();
                app.refresh_tab(&tab);
            }
        ));
        // Keeping mine writes the buffer over the file — gated on the version that was on screen
        // as Theirs, so a file that moved again while the panes were open is not overwritten
        // unseen: the banner stays up, and Compare shows the newer text.
        keep_mine.connect_clicked(glib::clone!(
            #[weak(rename_to = app)]
            self,
            #[weak]
            tab,
            move |_| {
                tab.leave_compare();
                match app.write_tab(&tab, Some(disk_etag)) {
                    Ok(()) => app.toast("Saved"),
                    Err(SaveError::ChangedOnDisk { .. }) => {
                        app.toast(&format!("{} changed on disk again", tab.rel()));
                    }
                    Err(e) => app.cannot("save", e),
                }
            }
        ));
        tab.compare(
            &format!("{rel} (unsaved)"),
            (&format!("{rel} (on disk)"), &disk),
            diff::Side::Old,
            true,
            Some(choice_row(&keep_theirs, &keep_mine)),
            "Changed on Disk",
        );
        tab.comparing_answers(Alert::Compare);
    }

    /// Raise or drop the conflict question on the tab showing `rel`, from what is on disk now.
    ///
    /// DESIGN.md, States: a conflict copy is a state that persists and needs a decision, so it is
    /// a banner on the file it concerns rather than a toast that scrolls past. It queues behind a
    /// "changed on disk" question rather than displacing it, and taking it down again brings that
    /// one back instead of clearing the bar.
    ///
    /// `trashed` is a copy this window has just sent to the trash. The index is a worker thread
    /// and a batch behind, so it still lists the file and the banner would otherwise linger until
    /// `FileRemoved` caught up a few hundred milliseconds later.
    ///
    /// The index is asked on a worker, because every text file that opens asks, and on a remote
    /// vault the asking is a round trip. A failed question is not an answer and changes nothing.
    pub fn sync_conflict_banner(self: &Rc<Self>, rel: &str, trashed: Option<&str>) {
        // Conflict copies are a vault idea: they are found by the index.
        let Some(vault) = self.vault().cloned() else {
            return;
        };
        if self.tab_for(rel).is_none() {
            return;
        }
        let (rel, trashed) = (rel.to_string(), trashed.map(str::to_string));
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let asked = rel.clone();
            let copies =
                crate::work::off_thread("conflict list", move || vault.conflicts_of(&asked)).await;
            let (Some(app), Some(Ok(copies))) = (weak.upgrade(), copies) else {
                return;
            };
            // The tab may have closed while the index was asked.
            let Some(tab) = app.tab_for(&rel) else {
                return;
            };
            match copies.iter().any(|copy| Some(copy) != trashed.as_ref()) {
                true => tab.show_alert(Alert::Conflict),
                false => tab.clear_alert(Alert::Conflict),
            }
        });
    }

    /// A sync conflict copy beside the note it was copied from, in the note's own tab: the editor
    /// is Mine, live, so an unsaved edit is in the comparison rather than older than it.
    ///
    /// The copy is read on a worker, being a round trip on a remote vault.
    fn resolve_conflict(self: &Rc<Self>, original: &str, conflict: &str) {
        let Some(vault) = self.vault().cloned() else {
            return;
        };
        let (original, conflict) = (original.to_string(), conflict.to_string());
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let asked = conflict.clone();
            let read =
                crate::work::attempt("read the conflict copy", move || vault.read(&asked)).await;
            let Some(app) = weak.upgrade() else {
                return;
            };
            match read {
                Ok((theirs, etag)) => app.compare_conflict(original, conflict, theirs, etag),
                Err(why) => app.toast(&why),
            }
        });
    }

    /// What [`resolve_conflict`](Self::resolve_conflict) does once the copy is read.
    fn compare_conflict(
        self: &Rc<Self>,
        original: String,
        conflict: String,
        theirs: String,
        theirs_etag: Etag,
    ) {
        let theirs_title = written_at(&conflict, &theirs_etag);
        let key = original.clone();
        self.with_tab(&key, Opened::Kept, "compare", move |app, tab| {
            let mine_title = match tab.save.etag.get() {
                Some(etag) => written_at(&original, &etag),
                None => original.clone(),
            };
            let keep_theirs = gtk::Button::with_label("Keep Theirs");
            let keep_mine = gtk::Button::with_label("Keep Mine");
            keep_mine.add_css_class("suggested-action");
            // Keeping theirs adopts the copy and reloads the tab over it: what was Mine, unsaved
            // edits included, was on screen and is the side the user gave up.
            keep_theirs.connect_clicked(glib::clone!(
                #[weak]
                app,
                #[weak]
                tab,
                #[strong]
                original,
                #[strong]
                conflict,
                move |_| {
                    tab.leave_compare();
                    app.keep_theirs(&tab, &original, &conflict);
                }
            ));
            // Keeping mine is only the copy going away: the merge is the buffer, and the buffer
            // saves as it always does, through the tab's own etag gate.
            keep_mine.connect_clicked(glib::clone!(
                #[weak]
                app,
                #[weak]
                tab,
                #[strong]
                original,
                #[strong]
                conflict,
                move |_| {
                    tab.leave_compare();
                    if tab.save.modified.get() {
                        app.save_tab(&tab, false);
                    }
                    app.finish_conflict(&original, &conflict);
                }
            ));
            tab.compare(
                &mine_title,
                (&theirs_title, &theirs),
                diff::Side::Old,
                true,
                Some(choice_row(&keep_theirs, &keep_mine)),
                "Sync Conflict",
            );
            tab.comparing_answers(Alert::Conflict);
        });
    }

    /// The copy takes the original's place and the tab reloads over it. The copy is adopted on a
    /// worker, being a round trip on a remote vault.
    fn keep_theirs(self: &Rc<Self>, tab: &Rc<Tab>, original: &str, conflict: &str) {
        let Some(vault) = self.vault().cloned() else {
            return;
        };
        let (original, conflict) = (original.to_string(), conflict.to_string());
        let (app, kept) = (Rc::downgrade(self), Rc::downgrade(tab));
        glib::spawn_future_local(async move {
            let adopted = crate::work::attempt("resolve", {
                let (original, conflict) = (original.clone(), conflict.clone());
                move || vault.adopt_conflict(&original, &conflict)
            })
            .await;
            let (Some(app), Some(tab)) = (app.upgrade(), kept.upgrade()) else {
                return;
            };
            if let Err(why) = adopted {
                return app.toast(&why);
            }
            tab.discard();
            app.refresh_tab(&tab);
            app.finish_conflict(&original, &conflict);
        });
    }

    /// The copy goes to the trash, and the banner is told before the index has seen it go.
    fn finish_conflict(self: &Rc<Self>, original: &str, conflict: &str) {
        if let Some(ops) = self.ops() {
            fileops::trash(ops, conflict);
        }
        self.sync_conflict_banner(original, Some(conflict));
    }

    /// Save As…: the file in front written under a path typed in the vault's path field, and its
    /// tab moved onto the new file (DESIGN.md, Keyboard). What the tab holds goes there, and the
    /// original keeps what was last written to it. Only a vault's own text, diagram, PDF or
    /// image: a loose tab, a window without a vault and a tab that is no file have no vault path
    /// to type, so there it does nothing.
    pub fn save_as(self: &Rc<Self>) {
        let Some(doc) = self.active_doc().filter(|doc| {
            matches!(
                doc,
                Doc::Text(_) | Doc::Diagram(_) | Doc::Pdf(_) | Doc::Image(_)
            ) && !doc.is_loose()
        }) else {
            return;
        };
        let Some(ops) = self.ops() else {
            return;
        };
        let app = Rc::downgrade(self);
        fileops::save_as(ops, &doc.key(), move |to| {
            if let Some(app) = app.upgrade() {
                app.save_as_to(doc, to);
            }
        });
    }

    /// Save As once the path is typed, looked at before anything is written: a folder is refused,
    /// a file asks to be replaced — saying so when a tab has it open, which then closes without
    /// saving — and a free path is written at once. The tab's own path is a plain Save.
    pub(crate) fn save_as_to(self: &Rc<Self>, doc: Doc, to: String) {
        if to == doc.key() {
            return self.save_active();
        }
        let Some(vault) = self.vault().cloned() else {
            return;
        };
        let app = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let taken = crate::work::off_thread("save as", {
                let to = to.clone();
                move || fileops::taken(&vault, &to)
            })
            .await;
            let (Some(app), Some(taken)) = (app.upgrade(), taken) else {
                return;
            };
            let name = doc::file_name(&to).to_string();
            match taken {
                fileops::Taken::Free => app.write_as(doc, to, true),
                fileops::Taken::Folder => app.cannot(&format!("save as {name}"), "it is a folder"),
                fileops::Taken::File => {
                    let open = app.doc_for(&to);
                    let mut body = format!("{to} already exists, and saving replaces it.");
                    if open.is_some() {
                        body.push_str(" It is open in a tab, which closes without saving.");
                    }
                    let weak = Rc::downgrade(&app);
                    dialogs::confirm(
                        &app.window,
                        &format!("Replace {name}?"),
                        &body,
                        "Replace",
                        true,
                        move || {
                            let Some(app) = weak.upgrade() else { return };
                            if let Some(open) = open {
                                app.close_unsaved(&open);
                            }
                            app.write_as(doc, to, false);
                        },
                    );
                }
            }
        });
    }

    /// Write what `doc` holds to `to`, then move its tab there. A save still on its way lands
    /// first, or its answer would come back to a tab that has moved, and a comparison goes, being
    /// about the old file. A note's relative paths are pointed back at what they named from its
    /// new folder. A PDF's strokes are written out and the file copied: its bytes are the render
    /// thread's. An image is copied as it is. `free` says nothing was at `to` when it was looked
    /// at.
    fn write_as(self: &Rc<Self>, doc: Doc, to: String, free: bool) {
        let Some(vault) = self.vault().cloned() else {
            return;
        };
        let (contents, edits) = match &doc {
            Doc::Text(tab) => {
                self.land_save(tab, false);
                tab.leave_compare();
                let note = tab.flavour().is_note();
                (Contents::Text(tab.for_disk(), note), tab.save.edits.get())
            }
            Doc::Diagram(diagram) => {
                self.land_diagram(diagram, false);
                diagram.finish_label();
                (
                    Contents::Text(diagram.for_disk(), false),
                    diagram.save.edits.get(),
                )
            }
            // No edit count: the strokes are all in the file once it is flushed.
            Doc::Pdf(pdf) => {
                pdf.flush_blocking();
                (Contents::File(pdf.path()), 0)
            }
            Doc::Image(_) => (Contents::Copy, 0),
            _ => return,
        };
        let from = doc.key();
        let name = doc::file_name(&to).to_string();
        let app = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            // The folders, then the file: each a round trip on a remote vault.
            let written = crate::work::off_thread("save as", {
                let (to, name) = (to.clone(), name.clone());
                move || {
                    fileops::make_parents(&vault, &to)?;
                    let written = match contents {
                        Contents::Text(text, note) => {
                            write_text(&vault, &from, &to, text, note, free).map(Some)
                        }
                        Contents::File(local) => copy_pdf(&vault, &from, &local, &to)
                            .map(|()| None)
                            .map_err(|e| e.to_string()),
                        Contents::Copy => vault
                            .copy(&from, &to)
                            .map(|()| None)
                            .map_err(|e| e.to_string()),
                    };
                    written.map_err(|why| format!("Cannot save as {name}: {why}"))
                }
            })
            .await;
            let Some(app) = app.upgrade() else {
                return;
            };
            match written {
                Some(Ok(wrote)) => {
                    app.follow_save_as(doc, &to, wrote, edits);
                    app.toast(&format!("Saved as {to}"));
                }
                Some(Err(why)) => app.toast(&why),
                None => app.cannot(&format!("save as {name}"), "the worker stopped"),
            }
        });
    }

    /// The tab once Save As has written `to`: reopened there when the extension changes what the
    /// file opens as, and otherwise pointed at it — clean, unless it was typed into while the
    /// write was out, which the next autosave then writes to the new file. `wrote` is a text
    /// write's etag, and whether the note's paths were rewritten on the way, which the tab then
    /// reloads as a rename's notes are.
    fn follow_save_as(
        self: &Rc<Self>,
        doc: Doc,
        to: &str,
        wrote: Option<(Etag, bool)>,
        edits: u64,
    ) {
        if doc::opens_differently(&doc.key(), to) {
            // What it held is in the new file, and the original keeps what it had.
            self.close_unsaved(&doc);
            return self.open_path(to);
        }
        doc.retarget(&self.root(), to);
        match (&doc, wrote) {
            (Doc::Text(tab), Some((etag, relinked))) => {
                // What the old file's banner said is not about the new one.
                tab.save.disk_changed.set(false);
                tab.clear_disk_alert();
                self.wrote(tab, etag, tab.save.edits.get() == edits);
                self.fetch_head(tab);
                // A buffer typed into meanwhile keeps its edits and gets the banner instead.
                if relinked {
                    self.refresh_tab(tab);
                }
            }
            (Doc::Diagram(diagram), Some((etag, _))) => {
                diagram.clear_changed();
                match diagram.save.edits.get() == edits {
                    true => diagram.mark_clean(etag),
                    false => diagram.save.etag.set(Some(etag)),
                }
            }
            // A refused upload of the old file left its copy's name for the next one to reuse.
            (Doc::Pdf(pdf), _) => pdf.clear_conflict(),
            _ => {}
        }
        self.sync_active();
        self.save_session_soon();
    }

    /// Close `doc` without writing what it has not saved: its file is being written over, or
    /// what it held is already in the file Save As wrote.
    fn close_unsaved(self: &Rc<Self>, doc: &Doc) {
        match doc {
            Doc::Text(tab) => tab.discard(),
            Doc::Diagram(diagram) => diagram.discard(),
            _ => {}
        }
        self.close_page(doc.page());
    }
}

/// What Save As writes: a tab's text, and whether it is a note's, a PDF's file as it is on this
/// machine, or an image's file, which the vault copies where it is: nothing of it is waiting on
/// this machine to be written, so on a remote vault too it is the host's copy of the host's file.
enum Contents {
    Text(String, bool),
    File(PathBuf),
    Copy,
}

/// A text tab's `text` at `to`, and whether it was rewritten: a note's relative paths pointed back
/// at what they named from `from`'s folder, where the index is, the host on a remote vault. A
/// free path is claimed first, as New File claims one, which also gives the file a new file's
/// mode rather than the write's private one.
fn write_text(
    vault: &Vault,
    from: &str,
    to: &str,
    text: String,
    note: bool,
    free: bool,
) -> Result<(Etag, bool), String> {
    let relinked = match note {
        true => vault
            .relink_copy(from, to, &text)
            .map_err(|e| format!("{e:#}"))?,
        false => None,
    };
    if free {
        vault.create_note(to, None).map_err(|e| format!("{e:#}"))?;
    }
    let etag = vault
        .save(to, relinked.as_deref().unwrap_or(&text), None)
        .map_err(|e| e.to_string())?;
    Ok((etag, relinked.is_some()))
}

/// A PDF's bytes at `to`. The vault copies the file where it is, except on a remote vault: there
/// the tab reads and writes the ssh cache copy, which reaches the host by a push of its own that
/// may still be on its way. So the cache copy goes up itself, and comes back down as `to`'s own
/// cache copy with its stamp, or the next stroke's push would take the host's file for somebody
/// else's change.
fn copy_pdf(vault: &Vault, from: &str, local: &Path, to: &str) -> std::io::Result<()> {
    match vault.is_remote() {
        false => vault.copy(from, to),
        true => vault
            .upload(local, to)
            .and_then(|()| vault.fetch(to))
            .map(drop),
    }
}

/// One write's outcome, on the `SAVES` target the save path has always logged to.
fn log_write(rel: &str, expected: Option<Etag>, written: &Result<Etag, SaveError>) {
    match written {
        Ok(etag) => tracing::debug!(target: SAVES, rel = %rel, ?expected, ?etag, "wrote"),
        Err(e) => tracing::debug!(target: SAVES, rel = %rel, ?expected, error = %e, "refused"),
    }
}
