//! Which cell is selected and where its caret is: a press in a cell or outside every cell, the
//! arrows going on past a cell's edge, and the view following the caret.

use std::rc::Rc;

use accent_core::notebook::CellId;
use adw::prelude::*;
use gtk::{glib, graphene};

use super::NotebookTab;
use super::cell::Editor;
use crate::panes::{NEARBY, Spot};

impl NotebookTab {
    pub(super) fn select(self: &Rc<Self>, id: CellId) {
        if self.selected.get() == Some(id) {
            return;
        }
        if let Some(old) = self.view_of(self.selected.get()) {
            old.set_selected(false);
        }
        if let Some(view) = self.view_of(Some(id)) {
            view.set_selected(true);
        }
        self.selected.set(Some(id));
        // Work in another cell than the one shown: the first cell in view holds the view again.
        self.shown.set(None);
        self.complete_in(id);
        self.place_soon();
        self.refade();
        self.history.borrow_mut().seal();
        self.on_select.emit(self);
    }

    /// A press in cell `id`, ahead of its editor's: the cell is selected, and the caret the editor
    /// puts down is not followed, being where the pointer is. A caret put down ten lines of the
    /// cells or more from where it was, in this cell or another, leaves a place to come back to.
    pub(super) fn press(self: &Rc<Self>, id: CellId) {
        let from = self.caret().map(|at| (at.buffer(), at.offset()));
        self.select(id);
        self.pressing.set(true);
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(tab) = weak.upgrade() {
                tab.pressing.set(false);
                if let Some((buffer, offset)) = from {
                    tab.leaped(&buffer.iter_at_offset(offset));
                }
            }
        });
        // A cell pressed before its editor was built: the editor takes the keyboard.
        if self.view_of(Some(id)).is_some_and(|v| v.editor().is_none()) {
            self.focus_cell(id);
        }
    }

    /// A press in the notebook outside every cell, between, beside or below them, leaves the cells
    /// ([`deselect`](Self::deselect)). The cell buttons are the cell's.
    pub(super) fn wire_outside(self: &Rc<Self>) {
        self.column.set_focusable(true);
        let click = gtk::GestureClick::new();
        click.connect_pressed(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_, _, x, y| {
                let Some(at) = tab.scroller.pick(x, y, gtk::PickFlags::DEFAULT) else {
                    return;
                };
                let within = |w: &gtk::Widget| at == *w || at.is_ancestor(w);
                let views = tab.views.borrow().clone();
                if !within(tab.tools.bar.upcast_ref())
                    && !views.iter().any(|v| within(v.card.upcast_ref()))
                {
                    tab.deselect();
                }
            }
        ));
        self.scroller.add_controller(click);
    }

    /// No cell selected, and the keyboard out of the one that was, in the notebook's column.
    pub(super) fn deselect(self: &Rc<Self>) {
        let Some(view) = self.view_of(self.selected.take()) else {
            return;
        };
        view.set_selected(false);
        self.column.grab_focus();
        self.place_soon();
        self.on_select.emit(self);
    }

    /// Select cell `id` and give its editor the keyboard.
    pub(super) fn focus_cell(self: &Rc<Self>, id: CellId) {
        self.select(id);
        if let Some((_, editor)) = self.editor(id) {
            editor.view.grab_focus();
            self.follow_caret(id);
        }
    }

    /// An arrow pressed in cell `id`: on its last row (`down`) or its first, the caret goes on
    /// into the cell beside it.
    pub(super) fn leave_cell(self: &Rc<Self>, id: CellId, down: bool) -> bool {
        // The arrows walk the completion popup's rows while it is up.
        if self.popup_shown(id) {
            return false;
        }
        let Some((_, editor)) = self.editor(id) else {
            return false;
        };
        let buffer = &editor.buffer;
        let at = buffer.iter_at_mark(&buffer.get_insert());
        let edge = match down {
            true => buffer.end_iter(),
            false => buffer.start_iter(),
        };
        let row = |iter: &gtk::TextIter| editor.view.iter_location(iter).y();
        row(&at) == row(&edge) && self.step_cell(id, down)
    }

    /// The caret went to where it is from `from`, in this cell's buffer or another's: ten lines of
    /// the cells or more away, the place it left is one to come back to.
    pub(super) fn leaped(self: &Rc<Self>, from: &gtk::TextIter) {
        let at = |spot: Option<Spot>| match spot {
            Some(Spot::Cell { at, .. }) => Some(at),
            _ => None,
        };
        let here = self.caret().and_then(|caret| at(self.spot_of(&caret)));
        if let (Some(from_at), Some(here)) = (at(self.spot_of(from)), here)
            && (from_at - here).abs() >= NEARBY
            && let Some(told) = self.on_leap.get()
        {
            told(from);
        }
    }

    /// Past the end of cell `id`, into the next cell's first line, or the previous cell's last.
    pub(super) fn step_cell(self: &Rc<Self>, id: CellId, down: bool) -> bool {
        let views = self.views.borrow().clone();
        let Some(at) = views.iter().position(|v| v.id == id) else {
            return false;
        };
        let next = match down {
            true => views.get(at + 1),
            false => at.checked_sub(1).and_then(|at| views.get(at)),
        };
        let Some((_, editor)) = next.and_then(|next| self.editor(next.id)) else {
            return false;
        };
        let buffer = &editor.buffer;
        let land = match down {
            true => buffer.start_iter(),
            false => buffer.end_iter(),
        };
        buffer.place_cursor(&land);
        if let Some(next) = next {
            self.focus_cell(next.id);
        }
        true
    }

    /// Where the caret is, as the window's history keeps a place: in the selected cell, if there
    /// is one, with its editor's buffer.
    pub fn caret(self: &Rc<Self>) -> Option<gtk::TextIter> {
        let (_, editor) = self.editor(self.selected.get()?)?;
        Some(editor.buffer.iter_at_mark(&editor.buffer.get_insert()))
    }

    /// The place of `iter` in a cell's buffer: the cell's place among the cells, its 1-based line
    /// and column in it, and the 0-based line of the cells joined that it is on.
    pub fn spot_of(&self, iter: &gtk::TextIter) -> Option<Spot> {
        let buffer = iter.buffer();
        let views = self.views.borrow();
        let cell = views.iter().position(|v| {
            v.editor()
                .is_some_and(|e| e.buffer.upcast_ref::<gtk::TextBuffer>() == &buffer)
        })?;
        let above: i32 = views[..cell].iter().map(|v| v.line_count()).sum();
        Some(Spot::Cell {
            cell,
            line: iter.line() + 1,
            column: iter.line_offset() + 1,
            at: above + iter.line(),
        })
    }

    /// Put the caret on `iter` in whichever cell's buffer holds it, as [`goto`](Self::goto) does;
    /// `false` where no cell does any more.
    pub fn goto_iter(self: &Rc<Self>, iter: &gtk::TextIter) -> bool {
        let buffer = iter.buffer();
        let id = self.views.borrow().iter().find_map(|v| {
            let editor = v.editor()?;
            (editor.buffer.upcast_ref::<gtk::TextBuffer>() == &buffer).then_some(v.id)
        });
        match id {
            Some(id) => {
                self.goto(id, iter.offset());
                true
            }
            None => false,
        }
    }

    /// Put the caret on 1-based `line` and `column` of the cell at place `cell`, or as near as the
    /// cells now come, as [`goto`](Self::goto) does.
    pub fn goto_place(self: &Rc<Self>, cell: usize, line: i32, column: i32) {
        let id = {
            let views = self.views.borrow();
            views.get(cell).or(views.last()).map(|v| v.id)
        };
        let Some((_, editor)) = id.and_then(|id| self.editor(id)) else {
            return;
        };
        let buffer = &editor.buffer;
        let line = (line - 1).clamp(0, buffer.line_count() - 1);
        let mut at = buffer.iter_at_line(line).unwrap_or(buffer.end_iter());
        if !at.ends_line() {
            let mut end = at;
            end.forward_to_line_end();
            at.set_line_offset((column - 1).clamp(0, end.line_offset()));
        }
        if let Some(id) = id {
            self.goto(id, at.offset());
        }
    }

    /// Keep cell `id`'s caret in sight: the cells do not scroll themselves, the notebook does.
    pub(super) fn follow_caret(self: &Rc<Self>, id: CellId) {
        if self.pressing.get() {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            let Some(tab) = weak.upgrade() else { return };
            let Some(editor) = tab.view_of(Some(id)).and_then(|v| v.editor().cloned()) else {
                return;
            };
            if editor.view.has_focus() {
                tab.scroll_to_caret(&editor, None);
            }
        });
    }

    /// Scroll so `editor`'s caret is `align` of the way down the view, or, with none, by as little
    /// as keeps it a margin inside.
    pub(super) fn scroll_to_caret(&self, editor: &Editor, align: Option<f64>) {
        let Some((top, bottom)) = self.caret_at(editor) else {
            return;
        };
        let adjustment = self.scroller.vadjustment();
        let (value, page, margin) = (adjustment.value(), adjustment.page_size(), 24.0);
        match align {
            Some(align) => adjustment.set_value(top - align * page),
            None if top < value => adjustment.set_value(top - margin),
            None if bottom > value + page => adjustment.set_value(bottom - page + margin),
            None => {}
        }
    }

    /// Bring cell `id`'s caret into view as [`scroll_to_caret`](Self::scroll_to_caret) does, once
    /// its editor is laid out: one built just now has no place to measure yet, and the keyboard
    /// may be elsewhere (the find bar), which [`follow_caret`](Self::follow_caret) waits for.
    pub(super) fn show(&self, id: CellId, align: Option<f64>) {
        self.reveal.set(Some((id, align)));
        self.scroller.queue_allocate();
    }

    /// The scroll [`show`](Self::show) asked for, in the first layout that has its editor in place.
    pub(super) fn show_pending(&self) {
        let Some((id, align)) = self.reveal.get() else {
            return;
        };
        match self.view_of(Some(id)).and_then(|v| v.editor().cloned()) {
            Some(editor) if editor.view.height() == 0 => {}
            Some(editor) => {
                self.reveal.set(None);
                self.scroll_to_caret(&editor, align);
                let value = self.scroller.vadjustment().value();
                self.shown.set(Some((id, value)));
            }
            None => self.reveal.set(None),
        }
    }

    /// The caret at character `at` of cell `id`, the keyboard in it, a quarter down the view:
    /// where Go to Cell and an Outline row land.
    pub fn goto(self: &Rc<Self>, id: CellId, at: i32) {
        self.focus_cell(id);
        if let Some((_, editor)) = self.editor(id) {
            editor
                .buffer
                .place_cursor(&editor.buffer.iter_at_offset(at));
        }
        self.show(id, Some(0.25));
    }

    /// The top and bottom of `view`'s caret in the scrolled content, where the scroll's value is
    /// measured, so a scroll not yet laid out does not move it.
    pub(super) fn caret_at(&self, editor: &Editor) -> Option<(f64, f64)> {
        let buffer = &editor.buffer;
        let insert = buffer.iter_at_mark(&buffer.get_insert());
        let (top, height) = line_in(editor.view.upcast_ref(), &insert, &self.content()?)?;
        Some((top, top + height))
    }

    /// The scrolled content, in whose coordinates the scroll's value is measured: the scroller's
    /// child is the viewport, and the content is the viewport's.
    pub(super) fn content(&self) -> Option<gtk::Widget> {
        self.scroller
            .child()
            .and_downcast::<gtk::Viewport>()?
            .child()
    }
}

/// The top and the height of the display line holding `iter` in `view`, in the scrolled
/// `content`'s coordinates.
pub(super) fn line_in(
    view: &gtk::TextView,
    iter: &gtk::TextIter,
    content: &gtk::Widget,
) -> Option<(f64, f64)> {
    let rect = view.iter_location(iter);
    let (x, y) = view.buffer_to_window_coords(gtk::TextWindowType::Widget, rect.x(), rect.y());
    let point = view.compute_point(content, &graphene::Point::new(x as f32, y as f32))?;
    Some((f64::from(point.y()), f64::from(rect.height())))
}
