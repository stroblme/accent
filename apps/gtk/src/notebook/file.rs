//! The notebook's file, which the tab writes what it holds to, reloads into a clean tab and
//! watches for another's writes, through the same save gate as a note's (`Saves`).

use std::path::PathBuf;
use std::rc::Rc;

use accent_core::fs::{Digest, Etag};
use accent_core::notebook::Notebook;
use adw::prelude::*;
use gtk::glib;

use super::NotebookTab;
use super::history::History;
use crate::editor::{SaveState, Saves};

impl NotebookTab {
    /// What the file should hold now, the cells' editors read into the model first: within what
    /// the reader takes, the largest outputs giving way, so that the notebook opens again.
    pub fn text(&self) -> String {
        self.sync_model();
        let (text, changed) = self
            .model
            .borrow_mut()
            .to_text_within(accent_core::fs::MAX_TEXT);
        for id in changed {
            self.redraw_cell(id);
        }
        text
    }

    /// The notebook as the tab holds it now, what was typed included: what Print… and Export as
    /// HTML… put on paper.
    pub fn snapshot(&self) -> Notebook {
        self.sync_model();
        self.model.borrow().clone()
    }

    /// A write landed: the tab is clean at `etag`.
    pub fn mark_clean(&self, etag: Etag) {
        self.save.etag.set(Some(etag));
        self.save.modified.set(false);
        self.set_title();
    }

    /// The edits are given up on (a tab closing without saving them).
    pub fn discard(&self) {
        self.save.modified.set(false);
        self.set_title();
    }

    /// Read the file again into a clean tab: the history starts over, as a note's does.
    pub fn reload(self: &Rc<Self>, notebook: Notebook, etag: Etag, digest: Digest) {
        *self.model.borrow_mut() = notebook;
        self.stale.take();
        self.pending.take();
        *self.history.borrow_mut() = History::default();
        // Every cell is new to the model read, so every view is built again.
        for view in self.views.take() {
            self.column.remove(&view.root);
            self.detach_cell(view.id);
        }
        self.selected.set(None);
        self.reconcile();
        self.save.etag.set(Some(etag));
        self.save.digest.set(Some(digest));
        self.save.modified.set(false);
        self.save.disk_changed.set(false);
        self.banner.set_revealed(false);
        self.set_title();
        self.on_history.emit(self);
        self.find_changed();
        self.language_changed();
    }

    /// The file moved under edits nobody has saved: the banner holds the question.
    pub fn show_changed(&self) {
        self.save.disk_changed.set(true);
        self.banner.set_revealed(true);
    }

    pub fn clear_changed(&self) {
        self.save.disk_changed.set(false);
        self.banner.set_revealed(false);
    }

    /// Watch the file behind this tab and call `f` when someone else writes it.
    pub fn watch_file(self: &Rc<Self>, f: impl Fn(&Rc<NotebookTab>) + 'static) {
        *self.monitor.borrow_mut() = crate::editor::watch_file(
            &self.path(),
            glib::clone!(
                #[weak(rename_to = tab)]
                self,
                move || f(&tab)
            ),
        );
    }
}

impl Saves for NotebookTab {
    fn save_state(&self) -> &SaveState {
        &self.save
    }

    fn key(&self) -> String {
        NotebookTab::key(self)
    }

    fn path(&self) -> PathBuf {
        NotebookTab::path(self)
    }

    fn for_disk(&self) -> String {
        self.text()
    }
}
