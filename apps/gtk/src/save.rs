//! Saving a tab and what happens when the disk disagrees: the etag check, the overwrite and
//! unsaved-changes questions, and resolving a sync conflict.

use super::*;

impl App {
    pub fn save_active(self: &Rc<Self>) {
        if let Some(tab) = self.active() {
            self.save_tab(&tab, true);
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
        if !editor::may_save(tab.modified.get(), tab.disk_changed.get()) {
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
        match self.write_tab(tab, tab.etag.get()) {
            Ok(()) => {
                self.sync_pdf_links_soon();
                if explicit {
                    self.toast("Saved");
                }
            }
            // The etag gate refused: the tab holds the question from now on, whatever asked. It
            // used to be recorded only for an autosave, so a Ctrl+S that was cancelled left a
            // blocked tab with no banner on it.
            Err(SaveError::ChangedOnDisk { .. }) => {
                tab.disk_changed.set(true);
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
            Err(SaveError::Offline) => {
                self.show_offline();
                if explicit {
                    self.toast("Not connected, so nothing was saved");
                }
            }
            Err(e) => self.cannot("save", e),
        }
    }

    /// Say the vault is unreachable, unless something already is.
    ///
    /// A save can notice the link is down before the connection thread has reported it, and the
    /// banner it would raise then must not overwrite the reason a real `Event::Disconnected` gave.
    fn show_offline(&self) {
        if !self.connection.is_revealed() {
            self.show_connection_banner("The vault is not answering");
        }
    }

    /// Write the buffer and hand the error back instead of reporting it: a caller that is about
    /// to make the buffer unreachable has to know whether the bytes landed.
    pub fn write_tab(&self, tab: &Rc<Tab>, expected: Option<Etag>) -> Result<(), SaveError> {
        // What the file should hold, not what the buffer holds: a code file loses its trailing
        // whitespace here and a DOS file gets its CRLFs back.
        let text = tab.for_disk();
        // A loose tab is not in any vault, so it writes through core directly. Same atomic save,
        // same etag gate; what it misses is the watcher being told the write was ours, which the
        // tab's own file monitor makes harmless.
        let written = match self.vault().filter(|_| !doc::is_loose_key(&tab.rel())) {
            Some(vault) => vault.save(&tab.rel(), &text, expected),
            None => accent_core::fs::write_note(&tab.path(), &text, expected),
        };
        match &written {
            Ok(etag) => tracing::debug!(target: SAVES, rel = %tab.rel(), ?expected, ?etag, "wrote"),
            Err(e) => {
                tracing::debug!(target: SAVES, rel = %tab.rel(), ?expected, error = %e, "refused");
            }
        }
        let etag = written?;
        tab.mark_clean(etag);
        tab.clear_disk_alert();
        // The tab is clean again, so the bar's dot goes with the one on the tab title.
        self.sync_status();
        lang::saved(tab);
        // Our own writes go through the vault, which tells the watcher they were ours, so no
        // event comes back to say the working tree moved. The pane is told here instead.
        if let Some(git) = self.git.get() {
            git.schedule_refresh(git::Depth::Status);
        }
        Ok(())
    }

    /// A watcher says the file under a tab moved.
    ///
    /// Whose write it was is the first question. Every save is a rename into place, which a file
    /// monitor reports as a change like anyone else's, so the etag is the only thing that tells
    /// our own writes apart from a real one: a file still carrying the etag we wrote holds
    /// exactly what the buffer already has. Reloading it anyway threw the view at the caret a
    /// second after every keystroke, and on a buffer typed into since the save it raised a
    /// "changed on disk" banner against our own bytes.
    ///
    /// Only for a watcher. Every other caller of [`Self::refresh_tab`] is answering a question
    /// the user was asked, and has to reload whatever the etag says.
    ///
    /// A stat that failed is not an answer and must not read as one. It used to fall in with "no
    /// file there", which differs from any etag we hold and so raised the banner: on a remote
    /// vault a dropped ssh connection would report a conflict over a diff holding nothing but the
    /// user's own edits. Nothing is lost by waiting — a real change fires the watcher again, and
    /// the etag gate refuses any save that would clobber one in the meantime.
    pub fn file_changed(self: &Rc<Self>, tab: &Rc<Tab>) {
        let looked = match self.vault().filter(|_| !doc::is_loose_key(&tab.rel())) {
            Some(vault) => vault.stat(&tab.rel()),
            None => match Etag::of(&tab.path()) {
                Ok(etag) => Ok(Some(etag)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            },
        };
        let disk = match looked {
            Ok(disk) => disk,
            Err(e) => {
                return tracing::debug!(
                    target: SAVES, rel = %tab.rel(), error = %e, "watcher: could not stat"
                );
            }
        };
        let ours = tab.etag.get();
        tracing::debug!(
            target: SAVES,
            rel = %tab.rel(),
            ?ours,
            ?disk,
            modified = tab.modified.get(),
            "watcher"
        );
        if ours != disk {
            self.refresh_tab(tab);
        }
    }

    /// Refresh a tab from what is on disk, unless its buffer holds edits nobody has saved: that
    /// buffer is the only copy of them, so the banner asks instead of the reload deciding.
    /// Returns whether the tab was refreshed.
    pub fn refresh_tab(self: &Rc<Self>, tab: &Rc<Tab>) -> bool {
        if tab.modified.get() {
            tab.disk_changed.set(true);
            tab.show_alert(Alert::Compare);
            return false;
        }
        // The read is off the main thread, so the status is asked for when the text lands rather
        // than here: a reload writes the buffer without an edit event, and nothing else would ask.
        tab.reload_keep_cursor(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move |_: &Rc<Tab>, read: std::io::Result<()>| {
                if let Err(e) = read {
                    app.cannot("reload", e);
                }
                app.sync_status();
            }
        ));
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
        dialog.choose(
            Some(&self.window),
            gio::Cancellable::NONE,
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
        let rel = tab.rel();
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

        let (app, tab) = (self.clone(), tab.clone());
        dialog.choose(
            Some(&self.window),
            gio::Cancellable::NONE,
            move |response| {
                let close = match response.as_str() {
                    "discard" => {
                        tab.discard();
                        true
                    }
                    "overwrite" => match app.write_tab(&tab, None) {
                        Ok(()) => true,
                        Err(e) => {
                            app.cannot("save", e);
                            false
                        }
                    },
                    _ => false,
                };
                after(&app, close);
            },
        );
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
        if let Some((pane, doc)) = self.pane_of(page).zip(self.doc_for_page(page)) {
            pane.nav.borrow_mut().forget(&doc.key());
        }
        self.docs.borrow_mut().retain(|d| d.page() != page);
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
            // another window, or by Syncthing, since the banner went up.
            Some(Alert::Conflict) => {
                let rel = tab.rel();
                match self
                    .vault()
                    .and_then(|v| v.conflicts_of(&rel).ok())
                    .unwrap_or_default()
                    .first()
                {
                    Some(conflict) => self.resolve_conflict(&rel, conflict),
                    None => {
                        tab.clear_alert(Alert::Conflict);
                        self.toast("The conflict copy is gone");
                    }
                }
            }
            None => tab.hide_banner(),
        }
    }

    /// The unsaved buffer against the file underneath it, in the tab itself: the editor is the
    /// Mine pane, so a merge is typed straight into the note.
    pub fn compare_with_disk(self: &Rc<Self>, tab: &Rc<Tab>) {
        let rel = tab.rel();
        let read = match self.vault().filter(|_| !doc::is_loose_key(&rel)) {
            Some(vault) => vault.read(&rel),
            None => accent_core::fs::read_note(&tab.path()),
        };
        let Ok((disk, disk_etag)) = read else {
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
    /// a banner on the note it concerns rather than a toast that scrolls past. It queues behind a
    /// "changed on disk" question rather than displacing it, and taking it down again brings that
    /// one back instead of clearing the bar.
    ///
    /// `trashed` is a copy this window has just sent to the trash. The index is a worker thread
    /// and a batch behind, so it still lists the file and the banner would otherwise linger until
    /// `FileRemoved` caught up a few hundred milliseconds later.
    pub fn sync_conflict_banner(&self, rel: &str, trashed: Option<&str>) {
        // Conflict copies are a vault idea: they are found by the index.
        if self.vault.is_none() {
            return;
        }
        let Some(tab) = self.tab_for(rel) else {
            return;
        };
        let standing = self
            .vault()
            .and_then(|v| v.conflicts_of(rel).ok())
            .unwrap_or_default()
            .iter()
            .any(|copy| Some(copy.as_str()) != trashed);
        match standing {
            true => tab.show_alert(Alert::Conflict),
            false => tab.clear_alert(Alert::Conflict),
        }
    }

    /// A sync conflict copy beside the note it was copied from, in the note's own tab: the editor
    /// is Mine, live, so an unsaved edit is in the comparison rather than older than it.
    fn resolve_conflict(self: &Rc<Self>, original: &str, conflict: &str) {
        let Some(vault) = self.vault() else {
            return;
        };
        let Ok((theirs, theirs_etag)) = vault.read(conflict) else {
            return self.toast("Cannot read the conflict copy");
        };
        let (original, conflict) = (original.to_string(), conflict.to_string());
        let theirs_title = written_at(&conflict, &theirs_etag);
        self.with_tab(&original.clone(), Opened::Kept, move |app, tab| {
            let mine_title = match tab.etag.get() {
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
                    let Some(vault) = app.vault() else { return };
                    if let Err(e) = vault.adopt_conflict(&original, &conflict) {
                        return app.cannot("resolve", e);
                    }
                    tab.discard();
                    app.refresh_tab(&tab);
                    app.finish_conflict(&original, &conflict);
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
                    if tab.modified.get() {
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

    /// The copy goes to the trash, and the banner is told before the index has seen it go.
    fn finish_conflict(&self, original: &str, conflict: &str) {
        if let Some(ops) = self.ops() {
            fileops::trash(ops, conflict);
        }
        self.sync_conflict_banner(original, Some(conflict));
    }
}
