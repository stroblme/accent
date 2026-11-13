//! The cell operations, from inserting a cell to joining two, each made through the model as one
//! step on the history.

use std::rc::Rc;

use accent_core::notebook::{Edit, Kind, Notebook};
use adw::prelude::*;

use super::NotebookTab;

impl NotebookTab {
    /// Make a cell operation through the model, on the history as one step.
    pub(super) fn operate(
        self: &Rc<Self>,
        op: impl FnOnce(&mut Notebook) -> Option<Vec<Edit>>,
    ) -> bool {
        if self.reading() {
            return false;
        }
        self.flush_pending();
        self.sync_model();
        let undo = op(&mut self.model.borrow_mut());
        let Some(undo) = undo.filter(|u| !u.is_empty()) else {
            return false;
        };
        self.history.borrow_mut().push_cells(undo);
        self.reconcile();
        self.changed();
        true
    }

    /// [`operate`](Self::operate) for an operation that makes cells, which fade in.
    fn operate_new(self: &Rc<Self>, op: impl FnOnce(&mut Notebook) -> Option<Vec<Edit>>) -> bool {
        self.fade_new.set(true);
        let done = self.operate(op);
        self.fade_new.set(false);
        done
    }

    /// The selected cell's place, if a cell is selected.
    pub(super) fn selected_at(&self) -> Option<usize> {
        self.position_of(self.selected.get()?)
    }

    /// A cell of `kind` below the selected one or above it; at the end with none selected.
    pub fn insert(self: &Rc<Self>, kind: Kind, below: bool) {
        let at = match (self.selected_at(), below) {
            (Some(at), true) => at + 1,
            (Some(at), false) => at,
            (None, _) => self.cell_count(),
        };
        self.insert_at(at, kind);
    }

    pub(super) fn insert_at(self: &Rc<Self>, at: usize, kind: Kind) {
        let mut new = None;
        self.operate_new(|nb| {
            let cell = nb.new_cell(kind, "")?;
            new = Some(cell.id());
            Some(nb.apply_all(vec![Edit::Insert { at, cell }]))
        });
        if let Some(id) = new {
            self.focus_cell(id);
        }
    }

    pub fn delete(self: &Rc<Self>) {
        let Some(id) = self.selected.get() else {
            return;
        };
        let at = self.position_of(id).unwrap_or(0);
        if self.operate(|nb| Some(nb.apply_all(vec![Edit::Remove { id }]))) {
            let next = {
                let model = self.model.borrow();
                let cells = model.cells();
                cells.get(at).or(cells.last()).map(|c| c.id())
            };
            match next {
                Some(next) => self.focus_cell(next),
                None => self.selected.set(None),
            }
        }
    }

    pub fn move_cell(self: &Rc<Self>, down: bool) {
        let (Some(id), Some(at)) = (self.selected.get(), self.selected_at()) else {
            return;
        };
        let to = match down {
            true => at + 1,
            false => match at.checked_sub(1) {
                Some(to) => to,
                None => return,
            },
        };
        if self.operate(|nb| Some(nb.apply_all(vec![Edit::Move { id, to }]))) {
            self.focus_cell(id);
        }
    }

    pub fn convert(self: &Rc<Self>, kind: Kind) {
        let Some(id) = self.selected.get() else {
            return;
        };
        if self.operate_new(|nb| nb.convert(id, kind)) {
            self.focus_cell(id);
        }
    }

    /// Split the selected cell at its caret.
    pub fn split(self: &Rc<Self>) {
        let Some((view, editor)) = self.selected.get().and_then(|id| self.editor(id)) else {
            return;
        };
        let caret = editor
            .buffer
            .iter_at_mark(&editor.buffer.get_insert())
            .offset();
        let text = editor.text();
        let at = text
            .char_indices()
            .nth(caret as usize)
            .map_or(text.len(), |(at, _)| at);
        let id = view.id;
        let mut new = None;
        self.operate_new(|nb| {
            let undo = nb.split(id, at)?;
            new = nb
                .position(id)
                .and_then(|p| nb.cells().get(p + 1))
                .map(|c| c.id());
            Some(undo)
        });
        if let Some(new) = new {
            self.focus_cell(new);
            if let Some((_, editor)) = self.editor(new) {
                editor.buffer.place_cursor(&editor.buffer.start_iter());
            }
        }
    }

    /// Join the selected cell with the one below it.
    pub fn join(self: &Rc<Self>) {
        let Some(id) = self.selected.get() else {
            return;
        };
        if self.operate(|nb| nb.join(id)) {
            self.focus_cell(id);
        }
    }
}
