//! Changes to the cells' text, each recorded on the notebook's one history as it is made, the
//! model brought up to date with what was typed and a write scheduled after them. Undo and Redo
//! walk that history back and forth.

use std::rc::Rc;
use std::time::Instant;

use accent_core::note::Syntax;
use accent_core::notebook::{CellId, Edit, Kind};
use adw::prelude::*;
use gtk::glib;

use super::NotebookTab;
use super::cell::{self, CellView};
use super::history::{Change, Step};
use super::kernel;

impl NotebookTab {
    /// A change to cell `id`'s text at character `at`, about to be made.
    pub(super) fn record(self: &Rc<Self>, id: CellId, at: i32, removed: &str, inserted: &str) {
        if self.replaying.get() {
            return;
        }
        let change = Change {
            at,
            removed: removed.to_string(),
            inserted: inserted.to_string(),
        };
        {
            let mut pending = self.pending.borrow_mut();
            match pending.as_mut() {
                Some((cell, changes)) if *cell == id => changes.push(change),
                _ => {
                    drop(pending);
                    self.flush_pending();
                    *self.pending.borrow_mut() = Some((id, vec![change]));
                }
            }
        }
        // A change made outside a user action is a step of its own.
        if self.acting.get() == 0 {
            self.flush_pending();
        }
    }

    /// The changes of the user action that ended become a step.
    pub(super) fn flush_pending(self: &Rc<Self>) {
        let Some((cell, changes)) = self.pending.take() else {
            return;
        };
        self.history
            .borrow_mut()
            .push_text(cell, changes, Instant::now());
        self.on_history.emit(self);
    }

    /// A code cell's last result may no longer describe its source.
    pub(super) fn source_changed(&self, view: &CellView, source: &str) {
        if view.kind == Kind::Code {
            let state = view.last_result.get().stale();
            view.last_result.set(state);
            let shown = if kernel::queued_for_source(self.kernel().as_deref(), view.id, source) {
                cell::Execution::Running
            } else {
                state
            };
            self.set_execution(view.id, shown);
        }
    }

    /// Cell `id`'s text changed.
    pub(super) fn edited(self: &Rc<Self>, id: CellId) {
        if self.replaying.get() {
            return;
        }
        if let Some(view) = self.view_of(Some(id)) {
            self.source_changed(&view, &view.text().unwrap_or_default());
        }
        {
            let mut stale = self.stale.borrow_mut();
            if !stale.contains(&id) {
                stale.push(id);
            }
        }
        if let Some(view) = self.view_of(Some(id))
            && let Some(editor) = view.editor()
            && view.kind == Kind::Markdown
        {
            crate::editor::style_note(&editor.view, &editor.buffer, Syntax::Markdown);
            self.sync_model();
            self.redeclare();
        }
        // Typing moves the caret without setting its mark.
        self.follow_caret(id);
        self.changed();
        self.typed(id);
    }

    /// Bring the model's sources up to date with what was typed.
    pub(super) fn sync_model(&self) {
        let stale = self.stale.take();
        if stale.is_empty() {
            return;
        }
        let mut model = self.model.borrow_mut();
        for id in stale {
            if let Some(text) = self.view_of(Some(id)).and_then(|v| v.text()) {
                model.apply(Edit::Source { id, text });
            }
        }
    }

    /// After any change, an undo included.
    pub(super) fn changed(self: &Rc<Self>) {
        self.mark_changed();
        self.save_soon();
    }

    /// Mark a run's output changed without scheduling a write on every stream flush.
    pub(super) fn mark_changed(self: &Rc<Self>) {
        self.zoom_anchor.replace(None);
        self.save.edits.set(self.save.edits.get() + 1);
        self.save.modified.set(true);
        self.set_title();
        self.on_history.emit(self);
        self.find_changed();
        self.language_changed();
    }

    pub(super) fn save_soon(self: &Rc<Self>) {
        self.autosave.call_once(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move || tab.on_autosave.emit(&tab)
        ));
    }

    pub fn undo(self: &Rc<Self>) {
        self.walk(true);
    }

    pub fn redo(self: &Rc<Self>) {
        self.walk(false);
    }

    fn walk(self: &Rc<Self>, back: bool) {
        if self.reading() {
            return;
        }
        self.flush_pending();
        let step = {
            let mut history = self.history.borrow_mut();
            match back {
                true => history.take_undo(),
                false => history.take_redo(),
            }
        };
        let Some(step) = step else {
            return;
        };
        let step = match step {
            Step::Text { cell, changes } => {
                self.replay_text(cell, &changes, back);
                Step::Text { cell, changes }
            }
            Step::Cells(edits) => {
                self.sync_model();
                let inverse = self.model.borrow_mut().apply_all(edits);
                self.reconcile();
                self.redraw_outputs(&inverse);
                Step::Cells(inverse)
            }
        };
        {
            let mut history = self.history.borrow_mut();
            match back {
                true => history.undone(step),
                false => history.redone(step),
            }
        }
        self.changed();
    }

    /// Walk a text step back or forward in its cell's buffer, the caret where it ends.
    fn replay_text(self: &Rc<Self>, id: CellId, changes: &[Change], back: bool) {
        let Some((view, editor)) = self.editor(id) else {
            return;
        };
        let buffer = &editor.buffer;
        let len = |s: &str| s.chars().count() as i32;
        let mut caret = 0;
        self.replay(|| {
            let mut apply = |change: &Change| {
                let (gone, put) = match back {
                    true => (&change.inserted, &change.removed),
                    false => (&change.removed, &change.inserted),
                };
                let (mut start, mut end) = (
                    buffer.iter_at_offset(change.at),
                    buffer.iter_at_offset(change.at + len(gone)),
                );
                buffer.delete(&mut start, &mut end);
                buffer.insert(&mut buffer.iter_at_offset(change.at), put);
                caret = change.at + len(put);
            };
            match back {
                true => changes.iter().rev().for_each(&mut apply),
                false => changes.iter().for_each(&mut apply),
            }
        });
        self.source_changed(&view, &editor.text());
        buffer.place_cursor(&buffer.iter_at_offset(caret));
        {
            let mut stale = self.stale.borrow_mut();
            if !stale.contains(&id) {
                stale.push(id);
            }
        }
        if view.kind == Kind::Markdown {
            crate::editor::style_note(&editor.view, buffer, Syntax::Markdown);
            self.sync_model();
            self.redeclare();
        }
        self.select(id);
        editor.view.grab_focus();
    }
}
