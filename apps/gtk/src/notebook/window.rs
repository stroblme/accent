//! Window integration for notebooks: opening one (a marimo `.py` known by its bytes, and a file
//! that does not read as a notebook opened as text saying why), the cell commands, Open as Text,
//! and saves through the same etag gate as notes.

use accent_api::Error;
use accent_core::Error as Core;
use accent_core::notebook::{self, Kind};

use super::NotebookTab;
use crate::*;

impl App {
    /// A file whose bytes are a notebook, its text read already by the text tab's opener: read
    /// as one on a worker and opened, or opened as text with a banner saying why it is not one.
    pub(crate) fn open_notebook_text(
        self: &Rc<Self>,
        key: &str,
        text: accent_core::fs::Text,
        flavour: editor::Flavour,
        how: Opened,
    ) {
        let key = key.to_string();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let read = crate::work::off_thread("notebook reader", {
                let key = key.clone();
                move || (notebook::read(&key, &text.text), text)
            })
            .await;
            let (Some(app), Some((read, text))) = (weak.upgrade(), read) else {
                return;
            };
            if app.doc_for(&key).is_some() {
                return;
            }
            match read {
                Ok(nb) => app.adopt_notebook(&key, nb, text, how),
                Err(e) => {
                    let tab = app.adopt_text_tab(&key, text, flavour, how);
                    tab.show_unread(&e.to_string());
                }
            }
        });
    }

    fn adopt_notebook(
        self: &Rc<Self>,
        key: &str,
        nb: notebook::Notebook,
        text: accent_core::fs::Text,
        how: Opened,
    ) {
        let path = self.root().join(key);
        let tab = super::open(
            key,
            &path,
            doc::file_name(key),
            &fileops::display_path(&self.root(), key),
            &self.tabs_for(key),
            (nb, text.etag, text.digest()),
        );
        self.wire_notebook(&tab);
        // A notebook inside the vault opens its code cells' program there; a loose one has none.
        tab.attach_language(self.vault().filter(|_| !doc::is_loose_key(key)).cloned());
        {
            let config = self.config.borrow();
            tab.set_font(config.editor_font.as_deref(), self.zoom.get());
            tab.set_spellcheck(config.spellcheck);
        }
        // Nothing else watches a loose file: the vault's worker only reports on its own tree.
        if doc::is_loose_key(key) {
            tab.watch_file(glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |tab| app.notebook_changed(tab)
            ));
        }
        let page = tab.page.clone();
        self.mark_loose(&page, key);
        self.watch_folder_of(key);
        self.docs.borrow_mut().push(Doc::Notebook(tab));
        self.select_new_page(&page, how);
        self.mark_opened(&page, how);
        self.save_session_soon();
    }

    fn wire_notebook(self: &Rc<Self>, tab: &Rc<NotebookTab>) {
        let weak = Rc::downgrade(self);
        let on = move |f: fn(&Rc<App>, &Rc<NotebookTab>)| {
            let weak = weak.clone();
            move |tab: &Rc<NotebookTab>| {
                if let Some(app) = weak.upgrade() {
                    f(&app, tab);
                }
            }
        };
        // An edit keeps a preview, as typing keeps a note's.
        tab.connect_history(on(|app, tab| {
            if tab.save.modified.get() {
                app.promote(&tab.page);
            }
            if app.is_active_notebook(tab) {
                app.sync_history();
                app.sync_status();
                app.outline_soon();
            }
        }));
        tab.connect_select(on(|app, tab| {
            if app.is_active_notebook(tab) {
                app.sync_status();
            }
        }));
        tab.connect_starting(on(|app, tab| {
            if app.is_active_notebook(tab) {
                app.sync_status();
            }
        }));
        tab.connect_follow(on(|app, _| app.go_to_definition()));
        self.wire_notebook_places(tab);
        tab.connect_autosave(on(|app, tab| app.save_notebook(tab, false)));
        tab.connect_banner(on(|app, tab| app.resolve_notebook(tab)));
        let weak = Rc::downgrade(self);
        tab.connect_zoom(move |out| {
            if let Some(app) = weak.upgrade() {
                app.set_zoom(crate::zoom::stepped_zoom(app.zoom.get(), out));
            }
        });
    }

    pub(crate) fn active_notebook(&self) -> Option<Rc<NotebookTab>> {
        self.active_doc()?.notebook().cloned()
    }

    fn is_active_notebook(&self, tab: &Rc<NotebookTab>) -> bool {
        self.active_notebook().is_some_and(|n| Rc::ptr_eq(&n, tab))
    }

    pub(crate) fn notebooks(&self) -> Vec<Rc<NotebookTab>> {
        self.docs
            .borrow()
            .iter()
            .filter_map(|d| d.notebook().cloned())
            .collect()
    }

    /// The notebook commands, over the notebook in front. `false` for a name that is not one.
    pub(crate) fn notebook_action(self: &Rc<Self>, name: &str) -> bool {
        let Some(tab) = self.active_notebook() else {
            return name.starts_with("notebook-");
        };
        match name {
            "notebook-undo" => tab.undo(),
            "notebook-redo" => tab.redo(),
            "notebook-insert-above" => tab.insert(Kind::Code, false),
            "notebook-insert-below" => tab.insert(Kind::Code, true),
            "notebook-insert-markdown" => tab.insert(Kind::Markdown, true),
            "notebook-delete-cell" => tab.delete(),
            "notebook-move-up" => tab.move_cell(false),
            "notebook-move-down" => tab.move_cell(true),
            "notebook-to-code" => tab.convert(Kind::Code),
            "notebook-to-markdown" => tab.convert(Kind::Markdown),
            "notebook-to-raw" => tab.convert(Kind::Raw),
            "notebook-split" => tab.split(),
            "notebook-join" => tab.join(),
            "notebook-run-cell" => self.run_selected(&tab, false),
            "notebook-run-advance" => self.run_selected(&tab, true),
            "notebook-run-all" => self.run_all(&tab),
            "notebook-interrupt" => self.interrupt_kernel(&tab),
            "notebook-restart" => self.restart_kernel(&tab),
            "notebook-shut-down" => self.shut_down_kernel(&tab),
            "notebook-clear-outputs" => tab.clear_outputs(false),
            "notebook-clear-all-outputs" => tab.clear_outputs(true),
            "notebook-open-text" => self.open_notebook_as_text(&tab),
            _ => return false,
        }
        true
    }

    /// Open as Text: the notebook's edits written, its tab closed, and the file opened as text.
    fn open_notebook_as_text(self: &Rc<Self>, tab: &Rc<NotebookTab>) {
        if let Err(e) = self.flush_notebook(tab) {
            return self.cannot("save", e);
        }
        let key = tab.key();
        let page = tab.page.clone();
        self.select_survivor(&page);
        self.forget_page(&page);
        self.close_page(&page);
        self.as_text.borrow_mut().insert(key.clone());
        self.open_path(&key);
    }

    /// Save through `App::start_save`, gated on the known etag: one write in flight, and none
    /// over a change on disk that edits are waiting on.
    pub(crate) fn save_notebook(self: &Rc<Self>, tab: &Rc<NotebookTab>, explicit: bool) {
        if !editor::may_save(tab.save.modified.get(), tab.save.disk_changed.get()) {
            if explicit && tab.save.disk_changed.get() {
                self.resolve_notebook(tab);
            }
            return;
        }
        let text = tab.text();
        self.start_save(
            tab,
            explicit,
            Self::save_notebook,
            Self::land_notebook,
            move || text,
        );
    }

    /// `App::land_flight` for a notebook: `report` is false for a write about to go after it.
    pub(crate) fn land_notebook(
        self: &Rc<Self>,
        tab: &Rc<NotebookTab>,
        report: bool,
    ) -> Option<bool> {
        self.land_notebook_saying(tab, report, "Saved")
    }

    /// [`land_notebook`](Self::land_notebook) saying `said` for a write asked for. A write that
    /// landed is what the disk holds, so no question of a change there is left.
    fn land_notebook_saying(
        self: &Rc<Self>,
        tab: &Rc<NotebookTab>,
        report: bool,
        said: &'static str,
    ) -> Option<bool> {
        self.land_flight(
            tab,
            Self::notebook_changed,
            move |app, tab, landed, explicit| match landed {
                editor::Landing::Clean(etag) => {
                    tab.mark_clean(etag);
                    tab.clear_changed();
                    tab.language_saved();
                    app.sync_status();
                    if report && explicit {
                        app.toast(said);
                    }
                }
                editor::Landing::Behind(etag) => {
                    tab.save.etag.set(Some(etag));
                    tab.clear_changed();
                }
                editor::Landing::Failed(Error::Core(Core::ChangedOnDisk { .. })) if report => {
                    tab.show_changed();
                    if explicit {
                        app.resolve_notebook(tab);
                    }
                }
                editor::Landing::Failed(e) if report && e.is_offline() => {
                    app.show_offline();
                    if explicit {
                        app.toast("Not connected. Changes were not saved");
                    }
                }
                editor::Landing::Failed(e) if report => app.cannot("save", e),
                editor::Landing::Failed(_) | editor::Landing::Stale => {}
            },
        )
    }

    /// Write the notebook now, gated on `expected`, and hand the error back: for a tab or a
    /// window on its way out, which has to know whether the bytes landed.
    pub(crate) fn write_notebook(
        self: &Rc<Self>,
        tab: &Rc<NotebookTab>,
        expected: Option<Etag>,
    ) -> Result<(), Error> {
        self.land_notebook(tab, false);
        let (etag, digest) = self.writer(tab)(&tab.text(), expected)?;
        tab.mark_clean(etag);
        tab.save.digest.set(Some(digest));
        tab.clear_changed();
        self.sync_status();
        Ok(())
    }

    /// [`write_notebook`](Self::write_notebook) at the tab's own etag, if it has edits.
    pub(crate) fn flush_notebook(self: &Rc<Self>, tab: &Rc<NotebookTab>) -> Result<(), Error> {
        self.land_notebook(tab, false);
        if !tab.save.modified.get() {
            return Ok(());
        }
        self.write_notebook(tab, tab.save.etag.get())
    }

    /// The watcher says the file under a notebook moved, and not by our own write: a notebook
    /// with no edits reloads, and one with edits asks.
    pub(crate) fn notebook_changed(self: &Rc<Self>, tab: &Rc<NotebookTab>) {
        self.check_disk(tab, |app, tab| match tab.save.modified.get() {
            true => tab.show_changed(),
            false => app.reload_notebook(tab),
        });
    }

    /// Read the file again into the tab, unless the tab changed while it was read
    /// ([`editor::Reloaded`]).
    pub(crate) fn reload_notebook(self: &Rc<Self>, tab: &Rc<NotebookTab>) {
        let vault = self
            .vault()
            .filter(|_| !doc::is_loose_key(&tab.key()))
            .cloned();
        let (key, path) = (tab.key(), tab.path());
        let reading = tab.save.begin_reload(&key);
        let (app, reloading) = (Rc::downgrade(self), Rc::downgrade(tab));
        glib::spawn_future_local(async move {
            let read = crate::work::off_thread("notebook reader", move || {
                let read = match vault {
                    Some(vault) => vault.read_text(&key),
                    None => accent_core::fs::read_text(&path).map_err(Into::into),
                };
                #[cfg(feature = "bench")]
                crate::bench::hold(&crate::bench::SLOW_RELOAD_MS);
                match read {
                    Ok(accent_core::fs::Read::Text(text)) => notebook::read(&key, &text.text)
                        .map(|nb| (nb, text.etag, text.digest()))
                        .map_err(|e| e.to_string()),
                    Ok(_) => Err("it is no longer a text file".to_string()),
                    Err(e) => Err(e.to_string()),
                }
            })
            .await;
            let (Some(app), Some(tab)) = (app.upgrade(), reloading.upgrade()) else {
                return;
            };
            match tab.save.reloaded(&reading, &tab.key()) {
                editor::Reloaded::Adopt => {}
                editor::Reloaded::Edited => return tab.show_changed(),
                editor::Reloaded::Superseded => return,
            }
            match read {
                Some(Ok((nb, etag, digest))) => {
                    tab.reload(nb, etag, digest);
                    app.sync_status();
                    app.sync_history();
                }
                Some(Err(why)) => app.cannot("reload", why),
                None => {}
            }
        });
    }

    /// Ask which version to keep when the disk changed under unsaved edits
    /// ([`App::ask_reload_or_overwrite`]).
    pub(crate) fn resolve_notebook(self: &Rc<Self>, tab: &Rc<NotebookTab>) {
        let (reloaded, written) = (tab.clone(), tab.clone());
        self.ask_reload_or_overwrite(
            "Notebook",
            &tab.key(),
            move |app| app.reload_notebook(&reloaded),
            move |app| app.overwrite_notebook(&written),
        );
    }

    /// Overwrite: the edits written over the disk's version on a worker, as a save is.
    fn overwrite_notebook(self: &Rc<Self>, tab: &Rc<NotebookTab>) {
        self.land_notebook(tab, false);
        let text = tab.text();
        self.start_write(
            tab,
            true,
            None,
            Self::save_notebook,
            |app, tab, report| app.land_notebook_saying(tab, report, "Overwritten"),
            move || text,
        );
        // Edits made while it is out wait on the question it answers until it lands: a save
        // is asked for after it, as one asked for meanwhile would be.
        tab.save.save_again.set(Some(false));
    }
}
