//! The cells' views in step with the model, one per cell in its order, each wired to the tab as
//! it is made and its editor built the first time something reaches into its text or its caret.

use std::rc::Rc;

use accent_core::notebook::{CellId, Kind};
use adw::prelude::*;
use gtk::{gdk, glib};
use sourceview5::prelude::ViewExt as _;

use super::NotebookTab;
use super::cell::{CellView, Editor};

impl NotebookTab {
    /// Bring the views in line with the model: one per cell in its order, of its kind, holding
    /// its source and rendered after the operators the cells declare. A view the model still has
    /// is kept, its caret and its outputs with it.
    pub(super) fn reconcile(self: &Rc<Self>) {
        self.redeclare();
        let declared = self.declared.borrow().clone();
        let mut old = self.views.take();
        let (mut views, mut new) = (Vec::new(), Vec::new());
        let cells: Vec<(CellId, Kind, String)> = self
            .model
            .borrow()
            .cells()
            .iter()
            .map(|c| (c.id(), c.kind(), c.source().to_string()))
            .collect();
        for (id, kind, source) in cells {
            let view = match old.iter().position(|v| v.id == id && v.kind == kind) {
                Some(i) => old.swap_remove(i),
                None => {
                    let view = self.build_view(id);
                    new.push(view.root.clone());
                    view
                }
            };
            if !view.shows_source(&source) {
                match view.editor() {
                    Some(editor) => self.replay(|| editor.buffer.set_text(&source)),
                    None => view.set_source(&source),
                }
                self.source_changed(&view, &source);
            }
            view.render_source(&self.key(), &source, &declared);
            views.push(view);
        }
        for gone in old {
            self.column.remove(&gone.root);
            self.detach_cell(gone.id);
        }
        let mut before: gtk::Widget = self.head.clone().upcast();
        for view in &views {
            if view.root.parent().is_none() {
                self.column.append(&view.root);
            }
            self.column.reorder_child_after(&view.root, Some(&before));
            before = view.root.clone().upcast();
            view.set_presenting(self.reading());
            view.set_selected(Some(view.id) == self.selected.get());
        }
        if self.fade_new.get() {
            new.iter().for_each(crate::widgets::fade_in);
        }
        *self.views.borrow_mut() = views;
        if self.view_of(self.selected.get()).is_none() {
            self.selected.set(None);
        }
        if self.view_of(self.hovered.get()).is_none() {
            self.hovered.set(None);
        }
        if self.view_of(self.gap_cell.get()).is_none() {
            self.gap_cell.set(None);
        }
        self.place_soon();
    }

    /// Read again the operators the markdown cells declare (`Notebook::declarations`), which every
    /// formula in the notebook is read after; where they changed, each output's formulas are read
    /// again. A rendered cell's page takes them at its next render ([`reconcile`]), the one way
    /// the cells of a notebook being read change.
    ///
    /// [`reconcile`]: Self::reconcile
    pub(super) fn redeclare(&self) {
        let declared = self.model.borrow().declarations();
        if *self.declared.borrow() == declared {
            return;
        }
        self.declared.replace(declared);
        let key = self.key();
        for view in self.views.borrow().iter() {
            view.outputs.redeclare(&key);
        }
    }

    pub(super) fn view_of(&self, id: Option<CellId>) -> Option<Rc<CellView>> {
        let id = id?;
        self.views.borrow().iter().find(|v| v.id == id).cloned()
    }

    pub(super) fn position_of(&self, id: CellId) -> Option<usize> {
        self.model.borrow().position(id)
    }

    /// The view of model cell `id`, wired to the tab; its editor comes when it is wanted
    /// ([`editor`](Self::editor)).
    fn build_view(self: &Rc<Self>, id: CellId) -> Rc<CellView> {
        let view = {
            let model = self.model.borrow();
            let at = model.position(id).expect("a cell of the model");
            Rc::new(CellView::new(
                &model.cells()[at],
                self.gutter.get(),
                &self.zoom,
                &self.declared,
            ))
        };
        // Read, its markdown outputs are rendered as they are built.
        if self.reading() {
            view.outputs.set_render(Some(self.output_render()));
        }
        let weak = Rc::downgrade(self);
        let on = move |f: fn(&Rc<NotebookTab>, CellId)| {
            let weak = weak.clone();
            move || {
                if let Some(tab) = weak.upgrade() {
                    f(&tab, id);
                }
            }
        };
        let click = gtk::GestureClick::new();
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let press = on(|tab, id| tab.press(id));
        click.connect_pressed(move |_, _, _, _| press());
        view.card.add_controller(click);
        // The buttons follow the pointer from cell to cell, its rail included, and the add strip
        // from gap to gap.
        let hover = |widget: &gtk::Widget, enter: fn(&Rc<NotebookTab>, CellId), leave| {
            let motion = gtk::EventControllerMotion::new();
            let (enter, leave) = (on(enter), on(leave));
            motion.connect_enter(move |_, _, _| enter());
            motion.connect_leave(move |_| leave());
            widget.add_controller(motion);
        };
        hover(
            view.row.upcast_ref(),
            |tab, id| tab.hover(id, true),
            |tab, id| tab.hover(id, false),
        );
        hover(
            view.gap.upcast_ref(),
            |tab, id| tab.hover_gap(id, true),
            |tab, id| tab.hover_gap(id, false),
        );
        view
    }

    /// Cell `id`'s view and its editor, built if need be: every way into a cell's text or its
    /// caret goes through here.
    pub(super) fn editor(self: &Rc<Self>, id: CellId) -> Option<(Rc<CellView>, Editor)> {
        let view = self.view_of(Some(id))?;
        let editor = match view.editor() {
            Some(editor) => editor.clone(),
            None => self.build_editor(&view),
        };
        Some((view, editor))
    }

    /// Build `view`'s editor over the model's text and wire it to the tab: the one place a
    /// cell's editor is made.
    pub(super) fn build_editor(self: &Rc<Self>, view: &Rc<CellView>) -> Editor {
        let id = view.id;
        let source = {
            let model = self.model.borrow();
            let at = model.position(id);
            at.map(|at| model.cells()[at].source().to_string())
                .unwrap_or_default()
        };
        let editor = view.build_editor(&source, self.language.as_ref(), self.reading());
        // A viewer's cells take no keyboard: its column reads with the reading keys.
        editor.view.set_focusable(!self.viewer);
        editor.view.set_tab_width(self.indent.get());
        crate::editor::refont(&editor.view);
        // A key that takes the caret far within the cell leaves a place to come back to; a click
        // is the notebook's to judge, which knows the cell the caret left (`press`).
        if let Some(column) = editor.view.downcast_ref::<crate::multicaret::View>() {
            let (weak, buffer) = (Rc::downgrade(self), editor.buffer.clone());
            column.on_leap(move |from, click| {
                if let Some(tab) = weak.upgrade().filter(|_| !click) {
                    tab.leaped(&buffer.iter_at_offset(from));
                }
            });
        }
        self.fade_from_selected(id, editor.view.upcast_ref());
        let weak = Rc::downgrade(self);
        let on = move |f: fn(&Rc<NotebookTab>, CellId)| {
            let weak = weak.clone();
            move || {
                if let Some(tab) = weak.upgrade() {
                    f(&tab, id);
                }
            }
        };
        let buffer = &editor.buffer;
        buffer.connect_insert_text({
            let weak = Rc::downgrade(self);
            move |_, at, text| {
                if let Some(tab) = weak.upgrade() {
                    tab.record(id, at.offset(), "", text);
                }
            }
        });
        buffer.connect_delete_range({
            let weak = Rc::downgrade(self);
            move |buffer, start, end| {
                if let Some(tab) = weak.upgrade() {
                    let removed = buffer.text(start, end, true);
                    tab.record(id, start.offset(), &removed, "");
                }
            }
        });
        buffer.connect_begin_user_action({
            let weak = Rc::downgrade(self);
            move |_| {
                if let Some(tab) = weak.upgrade() {
                    tab.acting.set(tab.acting.get() + 1);
                }
            }
        });
        buffer.connect_end_user_action({
            let weak = Rc::downgrade(self);
            move |_| {
                if let Some(tab) = weak.upgrade() {
                    tab.acting.set(tab.acting.get().saturating_sub(1));
                    if tab.acting.get() == 0 {
                        tab.flush_pending();
                    }
                }
            }
        });
        let edited = on(|tab, id| tab.edited(id));
        buffer.connect_changed(move |_| edited());
        let follow = on(|tab, id| {
            tab.follow_caret(id);
            tab.on_caret.emit(tab);
        });
        let occurrences = on(|tab, id| {
            tab.highlight_occurrences(id);
            tab.refade();
        });
        buffer.connect_mark_set(move |buffer, _, mark| {
            if *mark == buffer.get_insert() {
                follow();
            }
            if *mark == buffer.get_insert() || *mark == buffer.selection_bound() {
                occurrences();
            }
        });
        let focus = gtk::EventControllerFocus::new();
        let select = on(|tab, id| tab.select(id));
        focus.connect_enter(move |_| select());
        editor.view.add_controller(focus);
        // Down on a cell's last line goes on into the next cell, Up on its first back. Ahead of
        // the view, which keeps the caret where it is there.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed({
            let weak = Rc::downgrade(self);
            move |_, key, _, state| {
                let plain = (state & gtk::accelerator_get_default_mod_mask()).is_empty();
                let down = match key {
                    gdk::Key::Down | gdk::Key::KP_Down if plain => true,
                    gdk::Key::Up | gdk::Key::KP_Up if plain => false,
                    _ => return glib::Propagation::Proceed,
                };
                match weak.upgrade().is_some_and(|tab| tab.leave_cell(id, down)) {
                    true => glib::Propagation::Stop,
                    false => glib::Propagation::Proceed,
                }
            }
        });
        editor.view.add_controller(keys);
        if view.kind == Kind::Markdown && self.spellcheck.get() {
            crate::editor::set_spelling(&view.spell, &editor.buffer, &editor.view, true);
        }
        let query = self.occurrence.borrow().clone();
        crate::editor::paint_occurrences(&editor.buffer, &editor.occurrence, query.as_deref(), &[]);
        self.attach_cell(view, &editor);
        editor
    }

    /// Run `f`, which sets a buffer from the model or the history, recording nothing.
    pub(super) fn replay(&self, f: impl FnOnce()) {
        let was = self.replaying.replace(true);
        f();
        self.replaying.set(was);
    }
}
