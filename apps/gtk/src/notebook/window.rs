//! Window integration for notebooks: opening one (a marimo `.py` known by its bytes, and a file
//! that does not read as a notebook opened as text saying why), the cell commands, Open as Text,
//! and saves through the same etag gate as notes.

use accent_core::fs::Digest;
use accent_core::notebook::{self, Kind};

use super::NotebookTab;
use crate::save::SavesModel;
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
        let pick = self
            .vault()
            .and_then(|v| v.session().kernels.get(key).cloned());
        tab.set_kernel_pick(pick);
        // A notebook inside the vault opens its code cells' program there; a loose one has none.
        tab.attach_language(self.vault().filter(|_| !doc::is_loose_key(key)).cloned());
        {
            let config = self.config.borrow();
            tab.set_font(config.editor_font.as_deref(), self.zoom.get());
            tab.set_spellcheck(config.spellcheck);
            tab.set_ghost_text(config.ghost_text);
            tab.set_indent_width(config.indent_width);
        }
        // Nothing else watches a loose file: the vault's worker only reports on its own tree.
        if doc::is_loose_key(key) {
            tab.watch_file(glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |tab| app.model_changed(tab)
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
        // Typing in a cell fades the chrome, as typing in a note does, and leaves a place to come
        // back to, coalesced as a note's edits are.
        tab.connect_typed(on(|app, tab| {
            if app.is_active_notebook(tab) {
                app.mark_page(&tab.page);
                app.hide_chrome();
            }
        }));
        tab.connect_leap(glib::clone!(
            #[weak(rename_to = app)]
            self,
            #[weak]
            tab,
            move |from| app.leaped(&Doc::Notebook(tab.clone()), from)
        ));
        tab.connect_starting(on(|app, tab| {
            if app.is_active_notebook(tab) {
                app.sync_status();
            }
        }));
        tab.connect_follow(on(|app, _| app.go_to_definition()));
        self.wire_notebook_places(tab);
        tab.connect_autosave(on(|app, tab| app.save_model(tab, false)));
        tab.connect_banner(on(|app, tab| app.resolve_model(tab)));
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

    /// The notebook commands, over the notebook in front; the kernel's strip's, with none in
    /// front, over the Code pane's. `false` for a name that is not one.
    pub(crate) fn notebook_action(self: &Rc<Self>, name: &str) -> bool {
        let strip = matches!(
            name,
            "notebook-run-all"
                | "notebook-interrupt"
                | "notebook-restart"
                | "notebook-shut-down"
                | "notebook-clear-all-outputs"
        );
        let code = || self.code_doc().and_then(|doc| doc.notebook().cloned());
        let tab = self.active_notebook().or_else(|| code().filter(|_| strip));
        let Some(tab) = tab else {
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
        if let Err(e) = self.flush_model(tab) {
            return self.cannot("save", e);
        }
        let key = tab.key();
        let page = tab.page.clone();
        self.select_survivor(&page);
        self.forget_page(&page);
        self.close_page(&page);
        self.pending.as_text(&key);
        self.open_path(&key);
    }
}

impl SavesModel for NotebookTab {
    const KIND: &'static str = "Notebook";
    type Read = Result<(notebook::Notebook, Etag, Digest), String>;

    fn parse(key: &str, read: accent_api::Result<accent_core::fs::Read>) -> Self::Read {
        match read {
            Ok(accent_core::fs::Read::Text(text)) => notebook::read(key, &text.text)
                .map(|nb| (nb, text.etag, text.digest()))
                .map_err(|e| e.to_string()),
            Ok(_) => Err("it is no longer a text file".to_string()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn reloaded(app: &Rc<App>, tab: &Rc<Self>, read: Self::Read) {
        match read {
            Ok((nb, etag, digest)) => {
                tab.reload(nb, etag, digest);
                app.sync_status();
                app.sync_history();
            }
            Err(why) => app.cannot("reload", why),
        }
    }

    fn mark_clean(&self, etag: Etag) {
        NotebookTab::mark_clean(self, etag);
    }

    fn show_changed(&self) {
        NotebookTab::show_changed(self);
    }

    fn clear_changed(&self) {
        NotebookTab::clear_changed(self);
    }

    fn discard(&self) {
        NotebookTab::discard(self);
    }

    /// The code cells' language server hears of it, as it does of a code tab's save.
    fn saved(self: &Rc<Self>) {
        self.language_saved();
    }
}
