//! Save and reload a tab that saves a model rather than a buffer, a diagram or a notebook,
//! through the etag gate a note's save goes through. What differs between the two is
//! [`SavesModel`]: how the model is written out and read back, and who else hears of a save.

use super::*;

/// A tab whose file is its model written out whole, a diagram or a notebook: what its save and
/// reload ask of it. The gating, the landing, the questions and the flush are [`App`]'s.
pub(crate) trait SavesModel: Saves {
    /// What the conflict dialog calls it ("Diagram").
    const KIND: &'static str;
    /// A reload's read as [`SavesModel::parse`] leaves it.
    type Read: Send + 'static;

    /// What a save writes, taken now and finished on the worker.
    fn contents(&self) -> impl FnOnce() -> String + Send + 'static {
        let text = self.for_disk();
        move || text
    }

    /// The file at `key` as read for a reload, parsed on the worker.
    fn parse(key: &str, read: accent_api::Result<accent_core::fs::Read>) -> Self::Read;

    /// Put a reload's read in the tab, which nothing overtook while it was out, or say why not.
    fn reloaded(app: &Rc<App>, tab: &Rc<Self>, read: Self::Read);

    /// A write of the model landed: the tab is clean at `etag`.
    fn mark_clean(&self, etag: Etag);

    /// The file moved under edits nobody has saved: the banner holds the question.
    fn show_changed(&self);

    fn clear_changed(&self);

    /// The edits are given up on (a tab closing without saving them).
    fn discard(&self);

    /// A write landed, whatever was typed since: what else hears of it, as a note's server does.
    fn saved(self: &Rc<Self>) {}
}

impl App {
    /// Save through [`App::start_save`], gated on the known etag: one write in flight, and none
    /// over a change on disk that edits are waiting on.
    pub(crate) fn save_model<T: SavesModel>(self: &Rc<Self>, tab: &Rc<T>, explicit: bool) {
        let save = tab.save_state();
        if !editor::may_save(save.modified.get(), save.disk_changed.get()) {
            if explicit && save.disk_changed.get() {
                self.resolve_model(tab);
            }
            return;
        }
        let contents = tab.contents();
        self.start_save(tab, explicit, Self::save_model, Self::land_model, contents);
    }

    /// [`App::land_flight`] for a model: `report` is false for a write about to go after it.
    pub(crate) fn land_model<T: SavesModel>(
        self: &Rc<Self>,
        tab: &Rc<T>,
        report: bool,
    ) -> Option<bool> {
        self.land_model_saying(tab, report, "Saved")
    }

    /// [`land_model`](Self::land_model) saying `said` for a write asked for. A write that landed
    /// is what the disk holds, so no question of a change there is left.
    fn land_model_saying<T: SavesModel>(
        self: &Rc<Self>,
        tab: &Rc<T>,
        report: bool,
        said: &'static str,
    ) -> Option<bool> {
        self.land_flight(
            tab,
            Self::model_changed,
            move |app, tab, landed, explicit| match landed {
                editor::Landing::Clean(etag) => {
                    tab.mark_clean(etag);
                    tab.clear_changed();
                    tab.saved();
                    app.sync_status();
                    if report && explicit {
                        app.toast(said);
                    }
                }
                // Landed under newer edits, which the next save writes, as a note's does.
                editor::Landing::Behind(etag) => {
                    tab.save_state().etag.set(Some(etag));
                    tab.clear_changed();
                    tab.saved();
                    if report && explicit {
                        app.toast(said);
                    }
                }
                editor::Landing::Failed(Error::Core(Core::ChangedOnDisk { .. })) if report => {
                    tab.show_changed();
                    if explicit {
                        app.resolve_model(tab);
                    }
                }
                editor::Landing::Failed(e) if report && e.is_offline() => {
                    app.show_offline();
                    if explicit {
                        app.toast("Disconnected. Changes have not been saved");
                    }
                }
                editor::Landing::Failed(e) if report => app.cannot("save", e),
                editor::Landing::Failed(_) | editor::Landing::Stale => {}
            },
        )
    }

    /// Write the model now, gated on `expected`, and hand the error back: for a tab or a window
    /// on its way out, which has to know whether the bytes landed.
    fn write_model<T: SavesModel>(
        self: &Rc<Self>,
        tab: &Rc<T>,
        expected: Option<Etag>,
    ) -> Result<(), Error> {
        self.land_model(tab, false);
        let (etag, digest) = self.writer(tab)(&tab.for_disk(), expected)?;
        tab.mark_clean(etag);
        tab.save_state().digest.set(Some(digest));
        tab.clear_changed();
        tab.saved();
        self.sync_status();
        Ok(())
    }

    /// [`write_model`](Self::write_model) at the tab's own etag, if it has edits.
    pub(crate) fn flush_model<T: SavesModel>(self: &Rc<Self>, tab: &Rc<T>) -> Result<(), Error> {
        self.land_model(tab, false);
        if !tab.save_state().modified.get() {
            return Ok(());
        }
        self.write_model(tab, tab.save_state().etag.get())
    }

    /// The watcher says the file under a model moved, and not by our own write
    /// ([`App::check_disk`]): a tab with no edits reloads, and one with edits asks.
    pub(crate) fn model_changed<T: SavesModel>(self: &Rc<Self>, tab: &Rc<T>) {
        self.check_disk(tab, |app, tab| match tab.save_state().modified.get() {
            true => tab.show_changed(),
            false => app.reload_model(tab),
        });
    }

    /// Read the file again into the tab, unless the tab changed while it was read
    /// ([`editor::Reloaded`]).
    pub(crate) fn reload_model<T: SavesModel>(self: &Rc<Self>, tab: &Rc<T>) {
        let vault = self
            .vault()
            .filter(|_| !doc::is_loose_key(&tab.key()))
            .cloned();
        let (key, path) = (tab.key(), tab.path());
        let reading = tab.save_state().begin_reload(&key);
        let reader = format!("{} reader", T::KIND.to_lowercase());
        let (app, reloading) = (Rc::downgrade(self), Rc::downgrade(tab));
        glib::spawn_future_local(async move {
            let read = crate::work::off_thread(&reader, move || {
                let read = match vault {
                    Some(vault) => vault.read_text(&key),
                    None => accent_core::fs::read_text(&path).map_err(Into::into),
                };
                #[cfg(feature = "bench")]
                crate::bench::hold(&crate::bench::SLOW_RELOAD_MS);
                T::parse(&key, read)
            })
            .await;
            let (Some(app), Some(tab)) = (app.upgrade(), reloading.upgrade()) else {
                return;
            };
            match tab.save_state().reloaded(&reading, &tab.key()) {
                editor::Reloaded::Adopt => {}
                editor::Reloaded::Edited => return tab.show_changed(),
                editor::Reloaded::Superseded => return,
            }
            if let Some(read) = read {
                T::reloaded(&app, &tab, read);
            }
        });
    }

    /// Ask which version to keep when the disk changed under unsaved edits: there is no
    /// comparison of two diagrams or two notebooks to offer, as there is of two notes, so Reload
    /// throws the edits away and Overwrite the disk's version.
    pub(crate) fn resolve_model<T: SavesModel>(self: &Rc<Self>, tab: &Rc<T>) {
        let dialog = dialogs::alert(
            &format!("{} Changed on Disk", T::KIND),
            &format!(
                "{} changed outside Accent. Reload to discard your edits, or overwrite the saved version.",
                doc::file_name(&tab.key())
            ),
            &[
                ("cancel", "Cancel", adw::ResponseAppearance::Default),
                ("reload", "Reload", adw::ResponseAppearance::Destructive),
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
                "reload" => app.reload_model(&tab),
                "overwrite" => app.overwrite_model(&tab),
                _ => {}
            },
        );
    }

    /// Overwrite: the edits written over the disk's version on a worker, as a save is.
    fn overwrite_model<T: SavesModel>(self: &Rc<Self>, tab: &Rc<T>) {
        self.land_model(tab, false);
        let contents = tab.contents();
        self.start_write(
            tab,
            true,
            None,
            Self::save_model,
            |app, tab, report| app.land_model_saying(tab, report, "Overwritten"),
            contents,
        );
        // Edits made while it is out wait on the question it answers until it lands: a save
        // is asked for after it, as one asked for meanwhile would be.
        tab.save_state().save_again.set(Some(false));
    }

    /// [`App::ask_unsaved`] for a model on its way out.
    pub(crate) fn ask_unsaved_model<T: SavesModel>(
        self: &Rc<Self>,
        tab: &Rc<T>,
        error: &Error,
        after: impl Fn(&Rc<Self>, bool) + 'static,
    ) {
        let (discarded, written) = (tab.clone(), tab.clone());
        self.ask_unsaved_about(
            &tab.key(),
            error,
            move || discarded.discard(),
            move |app| app.write_model(&written, None),
            after,
        );
    }
}
