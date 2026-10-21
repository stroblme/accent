//! A notebook's Outline: its markdown cells' headings in the cells' order, the one the caret is
//! under followed through the cells.

use std::rc::Rc;

use accent_core::markdown;
use accent_core::notebook::{CellId, Kind};

use super::NotebookTab;
use crate::*;

impl NotebookTab {
    /// The Outline's rows: each markdown cell's headings, how deep, what they say and where they
    /// are, the cell and the heading's first character in it.
    pub fn outline(&self) -> Vec<(u8, String, (CellId, i32))> {
        self.sync_model();
        let model = self.model.borrow();
        let mut rows = Vec::new();
        for cell in model.cells().iter().filter(|c| c.kind() == Kind::Markdown) {
            let source = cell.source();
            for heading in markdown::analyze(source).headings {
                let at = source[..heading.range.start].chars().count() as i32;
                rows.push((heading.level, heading.text, (cell.id(), at)));
            }
        }
        rows
    }

    /// The row of the heading the caret is under, the last before it in its own cell or a
    /// markdown cell above, and whether the caret is above every heading.
    pub fn outline_row(&self) -> (Option<usize>, bool) {
        let rows = self.outline();
        let caret = self.selected.get().and_then(|id| {
            let editor = self.view_of(Some(id)).and_then(|v| v.editor().cloned());
            let at = editor.map_or(0, |e| {
                e.buffer.iter_at_mark(&e.buffer.get_insert()).offset()
            });
            Some((self.position_of(id)?, at))
        });
        let caret = caret.unwrap_or((0, 0));
        let row = rows.iter().rposition(|(_, _, (id, at))| {
            self.position_of(*id)
                .is_some_and(|cell| (cell, *at) <= caret)
        });
        (row, row.is_none() && !rows.is_empty())
    }
}

impl App {
    /// Fill the Outline again once a run of edits to the notebook in front is over, with the
    /// preview's and the word count's wait.
    pub(crate) fn outline_soon(self: &Rc<Self>) {
        self.refresh.call(glib::clone!(
            #[weak(rename_to = app)]
            self,
            move || app.sync_outline()
        ));
    }
}
