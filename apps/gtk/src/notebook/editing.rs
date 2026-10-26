//! The editor tab's commands in a notebook's cells: the editing chords on the selected cell's
//! view, the selection's other occurrences painted through every cell, spell checking in the
//! markdown cells, and a markdown cell's links followed (DESIGN.md, Keyboard › Editing).

use std::rc::Rc;

use accent_core::markdown::{self, Link};
use accent_core::notebook::{CellId, Kind};
use gtk::prelude::*;

use super::NotebookTab;

impl NotebookTab {
    /// The view the editing chords act on: the selected cell's, its editor built if need be.
    pub fn editing_view(self: &Rc<Self>) -> Option<sourceview5::View> {
        let (_, editor) = self.editor(self.selected.get()?)?;
        Some(editor.view)
    }

    /// What the selected cell edits as: a markdown cell as a note, the others as code.
    pub fn editing_flavour(&self) -> Option<crate::editor::Flavour> {
        let view = self.view_of(self.selected.get())?;
        Some(super::cell::flavour(view.kind))
    }

    /// Scroll Viewport: the notebook by `n` lines of the selected cell's text, the caret where it is.
    pub fn scroll_lines(self: &Rc<Self>, n: i32) {
        let Some(view) = self.editing_view() else {
            return;
        };
        let height = view
            .iter_location(&crate::editor::caret(&view.buffer()))
            .height();
        let adjustment = self.scroller.vadjustment();
        adjustment.set_value(adjustment.value() + f64::from(n * height));
    }

    /// Paste as Plain Text into the selected cell.
    pub fn paste_verbatim(self: &Rc<Self>) {
        if let Some(id) = self.selected.get()
            && self.editor(id).is_some()
            && let Some(view) = self.view_of(Some(id))
        {
            view.paste_verbatim();
        }
    }

    /// Spell check the markdown cells, or stop: the Preferences switch a note follows.
    pub fn set_spellcheck(&self, on: bool) {
        self.spellcheck.set(on);
        for view in self
            .views
            .borrow()
            .iter()
            .filter(|v| v.kind == Kind::Markdown)
        {
            if let Some(editor) = view.editor() {
                crate::editor::set_spelling(&view.spell, &editor.buffer, &editor.view, on);
            }
        }
    }

    /// Cell `id`'s caret or selection moved: its selection's other occurrences are painted in
    /// every cell with an editor, the notebook being one document as a code tab's file is, the
    /// selection itself left as GTK paints it.
    pub(super) fn highlight_occurrences(&self, id: CellId) {
        let Some(from) = self.view_of(Some(id)).and_then(|v| v.editor().cloned()) else {
            return;
        };
        let query = crate::editor::selected_occurrence(&from.buffer);
        if *self.occurrence.borrow() == query {
            return;
        }
        self.occurrence.replace(query.clone());
        let selected = crate::editor::selected_ranges(&from.view);
        for view in self.views.borrow().iter() {
            if let Some(editor) = view.editor() {
                let skip = if view.id == id { &selected[..] } else { &[] };
                crate::editor::paint_occurrences(
                    &editor.buffer,
                    &editor.occurrence,
                    query.as_deref(),
                    skip,
                );
            }
        }
    }

    /// The link under the selected markdown cell's caret, as the cell's text has it now.
    pub fn link_at_caret(&self) -> Option<Link> {
        let view = self.view_of(self.selected.get())?;
        let editor = view.editor().filter(|_| view.kind == Kind::Markdown)?;
        let text = editor.text();
        let caret = crate::editor::caret(&editor.buffer).offset() as usize;
        let at = text
            .char_indices()
            .nth(caret)
            .map_or(text.len(), |(at, _)| at);
        markdown::analyze(&text)
            .links
            .into_iter()
            .find(|link| link.range.contains(&at))
    }

    /// Put the caret on the heading or the `^block` `anchor` names in a markdown cell, as a
    /// note's `[[#Heading]]` goes to its own; `false` where no cell has it.
    pub fn goto_heading(self: &Rc<Self>, anchor: &str) -> bool {
        self.sync_model();
        let found = self.model.borrow().cells().iter().find_map(|cell| {
            let source = cell.source();
            let at = (cell.kind() == Kind::Markdown)
                .then(|| markdown::anchor_range(source, anchor))??;
            Some((cell.id(), source[..at.start].chars().count() as i32))
        });
        match found {
            Some((id, at)) => {
                self.goto(id, at);
                true
            }
            None => false,
        }
    }
}

/// What `ACCENT_BENCH_NOTEBOOK`'s parity step reads.
#[cfg(feature = "bench")]
impl NotebookTab {
    /// Cell `n`'s editor view, built if need be.
    pub fn bench_view(self: &Rc<Self>, n: usize) -> Option<sourceview5::View> {
        let id = self.views.borrow().get(n)?.id;
        Some(self.editor(id)?.1.view)
    }

    /// Where the character at `line`, `column` of cell `n` is drawn, across the notebook's
    /// column.
    pub fn bench_char_x(self: &Rc<Self>, n: usize, line: i32, column: i32) -> Option<f64> {
        let view = self.bench_view(n)?;
        let iter = view.buffer().iter_at_line_offset(line, column)?;
        let rect = view.iter_location(&iter);
        let (x, y) = view.buffer_to_window_coords(gtk::TextWindowType::Widget, rect.x(), rect.y());
        let point = gtk::graphene::Point::new(x as f32, y as f32);
        Some(f64::from(view.compute_point(&self.content()?, &point)?.x()))
    }

    /// How many cells paint an occurrence of the selection, and whether the selection itself is.
    pub fn bench_occurrences(&self) -> (usize, bool) {
        let painted = |editor: &super::cell::Editor| {
            let mut at = editor.buffer.start_iter();
            at.starts_tag(Some(&editor.occurrence))
                || at.forward_to_tag_toggle(Some(&editor.occurrence))
        };
        let views = self.views.borrow();
        let cells = views
            .iter()
            .filter_map(|v| v.editor())
            .filter(|e| painted(e))
            .count();
        let own = self
            .view_of(self.selected.get())
            .and_then(|v| v.editor().cloned())
            .and_then(|e| {
                let (start, _) = e.buffer.selection_bounds()?;
                Some(start.has_tag(&e.occurrence))
            });
        (cells, own.unwrap_or(false))
    }

    /// Whether cell `n` is spell checked.
    pub fn bench_spelled(&self, n: usize) -> bool {
        let view = self.views.borrow().get(n).cloned();
        view.and_then(|v| v.spell.borrow().as_ref().map(|a| a.is_enabled()))
            .unwrap_or(false)
    }
}
