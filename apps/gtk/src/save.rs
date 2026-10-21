//! Save tabs with etag checks and resolve disk or sync conflicts.

use super::*;
use crate::editor::Saves;
use accent_api::Error;
use accent_core::Error as Core;
use accent_core::fs::Digest;

impl App {
    pub fn save_active(self: &Rc<Self>) {
        if let Some(tab) = self.active() {
            self.save_tab(&tab, true);
        } else if let Some(diagram) = self.active_diagram() {
            self.save_diagram(&diagram, true);
        } else if let Some(notebook) = self.active_notebook() {
            self.save_notebook(&notebook, true);
        }
    }

    /// Save a tab; `explicit` permits a Ctrl+S dialog, while autosave never interrupts typing.
    ///
    /// [`editor::may_save`] blocks writes when the file moved underneath the buffer. Explicit
    /// saves show the refusal dialog; autosaves leave the existing banner.
    ///
    /// Tab and window closes use their own write-then-ask path: a refused closing write must
    /// keep the buffer available rather than lose unsaved text.
    pub fn save_tab(self: &Rc<Self>, tab: &Rc<Tab>, explicit: bool) {
        // A read-only tab, a lossy reading or an installed file, has nothing to write.
        if !tab.view.is_editable() {
            return;
        }
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
        let text = tab.for_disk();
        self.start_save(tab, explicit, Self::save_tab, Self::land_save, move || text);
    }

    /// Share asynchronous saving between notes and diagrams.
    ///
    /// `contents` prepares disk text on the worker; `land` applies results, and `again` repeats
    /// a save requested in flight. One write per tab avoids crossing remote round trips; queued
    /// saves use the returned etag when [`Self::land_flight`] applies the previous result.
    pub(crate) fn start_save<T: Saves>(
        self: &Rc<Self>,
        tab: &Rc<T>,
        explicit: bool,
        again: fn(&Rc<Self>, &Rc<T>, bool),
        land: fn(&Rc<Self>, &Rc<T>, bool) -> Option<bool>,
        contents: impl FnOnce() -> String + Send + 'static,
    ) {
        let expected = tab.save_state().etag.get();
        self.start_write(tab, explicit, expected, again, land, contents);
    }

    /// [`Self::start_save`] gated on `expected` rather than the tab's own etag.
    fn start_write<T: Saves>(
        self: &Rc<Self>,
        tab: &Rc<T>,
        explicit: bool,
        expected: Option<Etag>,
        again: fn(&Rc<Self>, &Rc<T>, bool),
        land: impl FnOnce(&Rc<Self>, &Rc<T>, bool) -> Option<bool> + 'static,
        contents: impl FnOnce() -> String + Send + 'static,
    ) {
        let save = tab.save_state();
        if save.flight.borrow().is_some() {
            let asked = save.save_again.get().unwrap_or(false);
            save.save_again.set(Some(asked || explicit));
            return;
        }
        let write = self.writer(tab);
        let (tx, answer) = std::sync::mpsc::channel();
        *save.flight.borrow_mut() = Some(editor::Flight {
            started: save.edits.get(),
            expected,
            held: save.etag.get(),
            explicit,
            answer,
        });
        // Started here and not inside the future below, which runs a turn of the loop later: a
        // write that has to wait for this one may be asked for in this same turn, and would wait
        // on a worker that had not begun. The answer goes through the channel; the worker's own
        // end is only the news that it is there.
        let worker = gio::spawn_blocking(move || tx.send(write(&contents(), expected)));
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

    /// Apply `tab`'s in-flight save, waiting if needed; return `None` if absent or its success.
    ///
    /// The first caller applies the result. A following write suppresses reporting because it
    /// will report any refusal itself. `apply` receives whether Ctrl+S requested the save;
    /// `recheck` handles a watcher stat deferred behind it.
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
            .unwrap_or_else(|_| Err(Core::Io("the save worker panicked".to_string()).into()));
        log_write(&tab.key(), flight.expected, &written);
        let digest = written.as_ref().ok().map(|(_, digest)| *digest);
        let landed = editor::landing(
            flight.started,
            save.edits.get(),
            flight.held,
            save.etag.get(),
            written.map(|(etag, _)| etag),
        );
        if matches!(
            landed,
            editor::Landing::Clean(_) | editor::Landing::Behind(_)
        ) {
            save.digest.set(digest);
        }
        let landed_ok = !matches!(landed, editor::Landing::Failed(_));
        apply(self, tab, landed, flight.explicit);
        if save.recheck.take() {
            recheck(self, tab);
        }
        Some(landed_ok)
    }

    /// A save that did not land, said the way its kind needs saying.
    fn refused(self: &Rc<Self>, tab: &Rc<Tab>, e: Error, explicit: bool) {
        match e {
            // The etag gate refused: the tab holds the question from now on, whatever asked. It
            // used to be recorded only for an autosave, so a Ctrl+S that was cancelled left a
            // blocked tab with no banner on it.
            Error::Core(Core::ChangedOnDisk { .. }) => {
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
            e if e.is_offline() => {
                self.show_offline();
                if explicit {
                    self.toast("Not connected. Changes were not saved");
                }
            }
            e => self.cannot("save", e),
        }
    }

    /// Report unreachable vault unless disconnection is already reported.
    /// A save can fail before the connection event and must not overwrite its later, specific reason.
    pub(crate) fn show_offline(&self) {
        if !self.connection.is_revealed() {
            self.show_connection_banner("The vault is not responding");
        }
    }

    /// Build a worker-side write returning its etag and content digest.
    ///
    /// The caller prepares disk text, including trimming and CRLF restoration in [`Tab::for_disk`].
    /// If the etag changed but bytes still match the previous read or write, retry at the new
    /// etag: metadata-only changes are not conflicts.
    ///
    /// Loose tabs use core's atomic etag-gated save directly; their own monitors recognise writes
    /// without the vault watcher notification.
    pub(crate) fn writer<T: Saves>(
        &self,
        tab: &Rc<T>,
    ) -> impl FnOnce(&str, Option<Etag>) -> Result<(Etag, Digest), Error> + Send + 'static {
        let (rel, path) = (tab.key(), tab.path());
        let vault = self.vault().filter(|_| !doc::is_loose_key(&rel)).cloned();
        let base = tab.save_state().digest.get();
        move |text, expected| {
            let write = |expected| match &vault {
                Some(vault) => vault.save(&rel, text, expected),
                None => accent_core::fs::write_note(&path, text, expected).map_err(Into::into),
            };
            let mut written = write(expected);
            if let Err(Error::Core(Core::ChangedOnDisk { .. })) = written
                && let Some(etag) = unchanged(vault.as_deref(), &rel, &path, base)
            {
                written = write(Some(etag));
            }
            written.map(|etag| (etag, accent_core::fs::digest(text)))
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

    /// Write the buffer before returning, landing any in-flight save first.
    /// Return errors to callers making the buffer unreachable so they can prevent data loss.
    pub fn write_tab(self: &Rc<Self>, tab: &Rc<Tab>, expected: Option<Etag>) -> Result<(), Error> {
        self.land_save(tab, false);
        let written = self.writer(tab)(&tab.for_disk(), expected);
        log_write(&tab.rel(), expected, &written);
        let (etag, digest) = written?;
        tab.save.digest.set(Some(digest));
        self.wrote(tab, etag, true);
        Ok(())
    }

    /// Write the buffer as the banner's explicit answer, gated on `expected` or any current etag with `None`.
    /// Land prior saves first, write on a worker, and show `said` after success.
    fn write_answer(self: &Rc<Self>, tab: &Rc<Tab>, expected: Option<Etag>, said: &'static str) {
        self.land_save(tab, false);
        let text = tab.for_disk();
        self.start_write(
            tab,
            true,
            expected,
            Self::save_tab,
            move |app, tab, report| app.land_answer(tab, report, said),
            move || text,
        );
    }

    /// [`Self::land_flight`] for [`Self::write_answer`]: the question is answered once the write
    /// is in, whatever was typed meanwhile, which the next save writes after it.
    fn land_answer(self: &Rc<Self>, tab: &Rc<Tab>, report: bool, said: &str) -> Option<bool> {
        self.land_flight(
            tab,
            Self::file_changed,
            |app, tab, landed, _| match landed {
                editor::Landing::Clean(etag) | editor::Landing::Behind(etag) => {
                    tab.save.disk_changed.set(false);
                    tab.clear_disk_alert();
                    app.wrote(tab, etag, matches!(landed, editor::Landing::Clean(_)));
                    if report {
                        app.toast(said);
                    }
                }
                editor::Landing::Failed(Error::Core(Core::ChangedOnDisk { .. })) if report => {
                    app.toast(&format!("{} changed on disk again", tab.rel()));
                }
                editor::Landing::Failed(e) if report => app.cannot("save", e),
                editor::Landing::Failed(_) | editor::Landing::Stale => {}
            },
        )
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

    /// Write using the tab's etag after landing in-flight saves, skipping a now-clean buffer.
    /// Used before closing or moving a dirty file.
    pub fn flush_tab(self: &Rc<Self>, tab: &Rc<Tab>) -> Result<(), Error> {
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

    /// Check watcher changes off-thread and call `moved` only for different file contents.
    ///
    /// Matching saved etags identify our atomic writes, avoiding spurious reloads and banners.
    /// Different etags with unchanged bytes update the etag without raising a conflict.
    ///
    /// Failed stats provide no answer, including remote disconnects. Leave state unchanged;
    /// future watcher events and the save etag gate still protect external changes. Reads and
    /// stats run on workers because remote calls cost round trips.
    pub(crate) fn check_disk<T: Saves>(self: &Rc<Self>, tab: &Rc<T>, moved: fn(&Rc<Self>, &Rc<T>)) {
        let (rel, path) = (tab.key(), tab.path());
        let vault = self.vault().filter(|_| !doc::is_loose_key(&rel)).cloned();
        let (ours, base) = (tab.save_state().etag.get(), tab.save_state().digest.get());
        let (app, watched) = (Rc::downgrade(self), Rc::downgrade(tab));
        glib::spawn_future_local(async move {
            let looked = crate::work::off_thread("stat", move || {
                let disk = match &vault {
                    Some(vault) => vault.stat(&rel)?,
                    None => match Etag::of(&path) {
                        Ok(etag) => Some(etag),
                        Err(Core::NotFound(_)) => None,
                        Err(e) => return Err(e.into()),
                    },
                };
                Ok(match disk {
                    Some(_) if disk != ours => {
                        let same = unchanged(vault.as_deref(), &rel, &path, base);
                        (disk, same)
                    }
                    _ => (disk, None),
                })
            })
            .await
            .unwrap_or_else(|| Err(Core::Io("the stat worker stopped".to_string()).into()));
            if let (Some(app), Some(tab)) = (app.upgrade(), watched.upgrade()) {
                app.compare_disk(&tab, ours, looked, moved);
            }
        });
    }

    /// What [`check_disk`](Self::check_disk) does once the stat is in: `ours` is the etag it
    /// looked against, and `looked` what is on disk and, where that is another etag over the
    /// same bytes, the etag they were read at.
    fn compare_disk<T: Saves>(
        self: &Rc<Self>,
        tab: &Rc<T>,
        ours: Option<Etag>,
        looked: accent_api::Result<(Option<Etag>, Option<Etag>)>,
        moved: fn(&Rc<Self>, &Rc<T>),
    ) {
        let save = tab.save_state();
        // A save still on its way has not put its etag in the tab yet, so our own write would
        // read as someone else's. Looked at again once it has landed.
        if save.flight.borrow().is_some() {
            save.recheck.set(true);
            return;
        }
        // A save or a reload landed meanwhile: the look was against what the tab held before.
        if save.etag.get() != ours {
            return self.check_disk(tab, moved);
        }
        let (disk, same) = match looked {
            Ok(looked) => looked,
            Err(e) => {
                return tracing::debug!(
                    target: SAVES, rel = %tab.key(), error = %e, "watcher: could not stat"
                );
            }
        };
        tracing::debug!(
            target: SAVES,
            rel = %tab.key(),
            ?ours,
            ?disk,
            ?same,
            modified = save.modified.get(),
            "watcher"
        );
        match same {
            Some(etag) => save.etag.set(Some(etag)),
            None if ours != disk => moved(self, tab),
            None => {}
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
            .filter(|_| !doc::is_loose_key(&tab.rel()) || doc::host_file_path(&tab.rel()).is_some())
            .cloned();
        // The read is off the main thread, so the status is asked for when the text lands rather
        // than here: a reload writes the buffer without an edit event, and nothing else would ask.
        tab.reload_keep_cursor(
            vault,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |_: &Rc<Tab>, read: accent_api::Result<()>| {
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
        // Compare rather than Reload: reloading threw the buffer away on one click, and the
        // resolver shows both sides and now lets them be merged by hand.
        let dialog = dialogs::alert(
            "File Changed on Disk",
            &format!("{} was modified elsewhere since you opened it.", tab.rel()),
            &[
                ("cancel", "Cancel", adw::ResponseAppearance::Default),
                ("compare", "Compare", adw::ResponseAppearance::Default),
                (
                    "overwrite",
                    "Overwrite",
                    adw::ResponseAppearance::Destructive,
                ),
            ],
            "cancel",
        );

        let (app, tab) = (self.clone(), tab.clone());
        dialogs::choose(
            &dialog,
            Some(&self.window),
            move |response| match response.as_str() {
                "compare" => app.compare_with_disk(&tab),
                "overwrite" => app.write_answer(&tab, None, "Overwritten"),
                _ => {}
            },
        );
    }

    /// Ask whether an unsavable closing tab may leave, then call `after` with the decision.
    /// Use an alert naming potential losses; save failure alone must never discard the buffer.
    pub fn ask_unsaved(
        self: &Rc<Self>,
        tab: &Rc<Tab>,
        error: &Error,
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
        error: &Error,
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

    /// The same question for a notebook on its way out.
    pub(crate) fn ask_unsaved_notebook(
        self: &Rc<Self>,
        tab: &Rc<crate::notebook::NotebookTab>,
        error: &Error,
        after: impl Fn(&Rc<Self>, bool) + 'static,
    ) {
        let (discarded, written) = (tab.clone(), tab.clone());
        self.ask_unsaved_about(
            &tab.key(),
            error,
            move || discarded.discard(),
            move |app| app.write_notebook(&written, None),
            after,
        );
    }

    /// The question itself, whatever kind of document holds the edits: `discard` throws them
    /// away, `overwrite` writes them over whatever is on disk.
    fn ask_unsaved_about(
        self: &Rc<Self>,
        rel: &str,
        error: &Error,
        discard: impl FnOnce() + 'static,
        overwrite: impl FnOnce(&Rc<Self>) -> Result<(), Error> + 'static,
        after: impl Fn(&Rc<Self>, bool) + 'static,
    ) {
        let body = match error {
            Error::Core(Core::ChangedOnDisk { .. }) => {
                format!("{rel} changed on disk, so your edits could not be saved.")
            }
            e => format!("{rel} could not be saved: {e}"),
        };
        // Overwriting means writing again, which is the very thing that just failed for want of a
        // connection: offering it would be offering nothing.
        let mut responses = vec![
            ("cancel", "Cancel", adw::ResponseAppearance::Default),
            ("discard", "Discard", adw::ResponseAppearance::Destructive),
        ];
        if !error.is_offline() {
            responses.push(("overwrite", "Overwrite", adw::ResponseAppearance::Default));
        }
        let dialog = dialogs::alert("Unsaved Changes", &body, &responses, "cancel");

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
        // A drawn-on document leaving: the write lands after the tab has gone from the window,
        // and on a remote vault still goes up.
        if let Some(Doc::Pdf(pdf)) = self.doc_for_page(page) {
            pdf.flush_closing();
            if let Some(at) = pdf.ring_at() {
                self.ring_at.set(Some(at));
            }
            crate::malloc::trim_soon();
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
        // A notebook closing was asked about already (`wire.rs`), as a diagram was; its kernel
        // ends with its tab.
        if let Some(Doc::Notebook(n)) = self.doc_for_page(page) {
            self.stop_kernel(&n);
            if let Err(e) = self.flush_notebook(&n) {
                tracing::debug!("a closing notebook was not written: {e}");
            }
        }
        // Closing a shell's tab is ending it; closing its window only lets go of it, and a window
        // being destroyed closes no page through here. A shell is refused a move to another
        // window before it would reach here too.
        if let Some(Doc::Terminal(term)) = self.doc_for_page(page) {
            term.kill();
        }
        // A file's breakpoints outlive its tab, for when it is opened again.
        if let Some(Doc::Text(tab)) = self.doc_for_page(page) {
            self.debugger.closing(&tab);
        }
        // Every pane's: a tab moved out of one leaves its places there, and a caret's place
        // holds a mark in its buffer.
        if let Some(doc) = self.doc_for_page(page) {
            for pane in self.panes.borrow().iter() {
                pane.nav.borrow_mut().forget(&doc.key());
            }
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
            Some(Alert::ReadOnly | Alert::Installed | Alert::Host | Alert::Unread) => {}
            // Both sides hold work, so neither is thrown away on one click: the diff shows what
            // differs and the user picks (DESIGN.md: a choice that can lose data is a dialog).
            Some(Alert::Compare) => self.compare_with_disk(tab),
            Some(Alert::Restore) => self.write_answer(tab, None, "Saved"),
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
                None => accent_core::fs::read_note(&path).map_err(Into::into),
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
                app.write_answer(&tab, Some(disk_etag), "Saved");
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

    /// Update the persistent conflict banner on the tab showing `rel`.
    ///
    /// Conflict copies need a decision, so banners retain them and queue behind changed-on-disk
    /// questions. `trashed` omits a copy removed by this window before the index catches up.
    /// Query on a worker; failures leave the banner unchanged.
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

    /// Save the active vault text, diagram, PDF or image under a new vault path and move its tab.
    /// The original retains its last saved contents. Loose files and non-file tabs have no vault Save As.
    pub fn save_as(self: &Rc<Self>) {
        let Some(doc) = self.active_doc().filter(|doc| {
            matches!(
                doc,
                Doc::Text(_) | Doc::Diagram(_) | Doc::Notebook(_) | Doc::Pdf(_) | Doc::Image(_)
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

    /// Validate Save As destination: reject folders, confirm file replacement, save immediately if absent.
    /// Replacing an open destination closes it without saving; choosing the current path performs Save.
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

    /// Save `doc` to `to` and move its tab, after applying any in-flight save.
    ///
    /// Close comparisons of the old file and rebase note-relative links. PDFs flush strokes
    /// before copying render-owned bytes; images copy unchanged. `free` records that `to` was
    /// absent when checked.
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
            Doc::Notebook(notebook) => {
                self.land_notebook(notebook, false);
                (
                    Contents::Text(notebook.for_disk(), false),
                    notebook.save.edits.get(),
                )
            }
            // No edit count: the strokes are all in the file once it is flushed.
            Doc::Pdf(pdf) => {
                pdf.flush_blocking(false);
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
                        Contents::File(local) => copy_pdf(&vault, &from, &local, &to, free)
                            .map(|()| None)
                            .map_err(|e| e.to_string()),
                        // A copy never replaces, so the Replace confirmed writes over instead.
                        Contents::Copy => match free {
                            true => vault.copy(&from, &to),
                            false => vault
                                .fetch(&from)
                                .and_then(|local| vault.upload(&local, &to, true)),
                        }
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

    /// Adopt Save As at `to`, reopening if its extension changes kind, otherwise retargeting the tab.
    /// Edits made during writing stay dirty for the next save. `wrote` supplies text etag/digest
    /// and whether rebased note paths require reloading.
    fn follow_save_as(
        self: &Rc<Self>,
        doc: Doc,
        to: &str,
        wrote: Option<(Etag, Digest, bool)>,
        edits: u64,
    ) {
        if doc::opens_differently(&doc.key(), to) {
            // What it held is in the new file, and the original keeps what it had.
            self.close_unsaved(&doc);
            return self.open_path(to);
        }
        doc.retarget(&self.root(), to);
        match (&doc, wrote) {
            (Doc::Text(tab), Some((etag, digest, relinked))) => {
                // What the old file's banner said is not about the new one.
                tab.save.disk_changed.set(false);
                tab.clear_disk_alert();
                tab.save.digest.set(Some(digest));
                self.wrote(tab, etag, tab.save.edits.get() == edits);
                self.fetch_head(tab);
                // A buffer typed into meanwhile keeps its edits and gets the banner instead.
                if relinked {
                    self.refresh_tab(tab);
                }
            }
            (Doc::Diagram(diagram), Some((etag, digest, _))) => {
                diagram.clear_changed();
                diagram.save.digest.set(Some(digest));
                match diagram.save.edits.get() == edits {
                    true => diagram.mark_clean(etag),
                    false => diagram.save.etag.set(Some(etag)),
                }
            }
            (Doc::Notebook(notebook), Some((etag, digest, _))) => {
                notebook.clear_changed();
                notebook.save.digest.set(Some(digest));
                match notebook.save.edits.get() == edits {
                    true => notebook.mark_clean(etag),
                    false => notebook.save.etag.set(Some(etag)),
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
            Doc::Notebook(notebook) => notebook.discard(),
            _ => {}
        }
        self.close_page(doc.page());
    }
}

/// Save As content: prepared text, local PDF cache bytes or vault-owned image bytes.
/// Remote image copies stay host-side because no pending edits exist locally.
enum Contents {
    Text(String, bool),
    File(PathBuf),
    Copy,
}

/// Write text from `from` to `to`, returning its digest and whether note paths were rebased.
/// Claim free destinations first to preserve normal new-file permissions; remote rebasing runs on the host.
fn write_text(
    vault: &Vault,
    from: &str,
    to: &str,
    text: String,
    note: bool,
    free: bool,
) -> Result<(Etag, Digest, bool), String> {
    let relinked = match note {
        true => vault
            .relink_copy(from, to, &text)
            .map_err(|e| format!("{e:#}"))?,
        false => None,
    };
    if free {
        vault.create_note(to, None).map_err(|e| format!("{e:#}"))?;
    }
    let written = relinked.as_deref().unwrap_or(&text);
    let etag = vault.save(to, written, None).map_err(|e| e.to_string())?;
    Ok((etag, accent_core::fs::digest(written), relinked.is_some()))
}

/// Save PDF bytes to `to`, using the local edited cache for remote vaults: copied to a `free`
/// name, written over one whose Replace was confirmed, since a copy never replaces.
/// Upload and fetch the destination with its cache stamp so later strokes do not mistake the
/// new file for an external change.
fn copy_pdf(
    vault: &Vault,
    from: &str,
    local: &Path,
    to: &str,
    free: bool,
) -> accent_api::Result<()> {
    match (vault.is_remote(), free) {
        (false, true) => vault.copy(from, to),
        (false, false) => vault.upload(local, to, true),
        (true, _) => vault
            .upload(local, to, !free)
            .and_then(|()| vault.fetch(to))
            .map(drop),
    }
}

/// The etag of the file at `rel` (at `path` outside a vault) if it still holds what `base`
/// digests, its etag having moved alone; `None` if it holds anything else or cannot be read.
fn unchanged(vault: Option<&Vault>, rel: &str, path: &Path, base: Option<Digest>) -> Option<Etag> {
    let base = base?;
    let read = match vault {
        Some(vault) => vault.read_text(rel),
        None => accent_core::fs::read_text(path).map_err(Into::into),
    };
    match read {
        Ok(accent_core::fs::Read::Text(text)) if text.digest() == base => Some(text.etag),
        _ => None,
    }
}

/// One write's outcome, on the `SAVES` target the save path has always logged to.
fn log_write(rel: &str, expected: Option<Etag>, written: &Result<(Etag, Digest), Error>) {
    match written {
        Ok((etag, _)) => tracing::debug!(target: SAVES, rel = %rel, ?expected, ?etag, "wrote"),
        Err(e) => tracing::debug!(target: SAVES, rel = %rel, ?expected, error = %e, "refused"),
    }
}
