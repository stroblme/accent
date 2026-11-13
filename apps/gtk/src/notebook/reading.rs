//! A notebook that is only read, presented or as a preview tab's viewer following its notebook.
//! Its markdown cells and outputs are rendered on pages of their own, and the reading keys scroll
//! it as a note's preview is scrolled.

use std::rc::Rc;

use accent_core::notebook::{CellId, Kind, Notebook};
use adw::prelude::*;
use gtk::{gdk, glib};

use super::NotebookTab;
use super::cell::{self, CellView};
use super::output;

impl NotebookTab {
    /// Whether the notebook is only read: a viewer, or presented.
    pub(super) fn reading(&self) -> bool {
        self.viewer || self.presenting.get()
    }

    /// Whether cell `id` is read rendered: a markdown cell of a notebook being read.
    pub(super) fn renders(&self, id: CellId) -> bool {
        self.reading()
            && self
                .view_of(Some(id))
                .is_some_and(|v| v.kind == Kind::Markdown)
    }

    /// Read only, the buttons and the inserters away, and the markdown cells rendered, while
    /// presented; their pages go afterwards, the cells' sources showing again.
    pub fn set_presenting(self: &Rc<Self>, on: bool) {
        if self.viewer || self.presenting.replace(on) == on {
            return;
        }
        self.head.set_visible(!on);
        let views = self.views.borrow().clone();
        for view in &views {
            view.set_presenting(on);
            view.outputs.set_render(on.then(|| self.output_render()));
            if !on {
                view.drop_rendered();
            }
        }
        // The cells in view at once, so presenting shows them rendered from its first frames.
        if on {
            for view in self.near.borrow().clone() {
                self.render_cell(&view);
            }
        }
        self.built_at.set(-1);
        self.scroller.queue_allocate();
        self.find_changed();
        self.place_soon();
    }

    /// A viewer takes the notebook as it is now, `notebook` its source's snapshot and `queued`
    /// the cells its kernel has yet to finish, which count `[*]`: each cell's text and rendered
    /// page brought up to date, and a code cell's outputs drawn again from the first that changed,
    /// so a stream that grew only grows.
    pub fn follow(self: &Rc<Self>, notebook: Notebook, queued: &[CellId]) {
        let old = self.model.replace(notebook);
        self.reconcile();
        {
            let model = self.model.borrow();
            for cell in model.cells() {
                let Some(view) = self.view_of(Some(cell.id())) else {
                    continue;
                };
                match queued.contains(&cell.id()) {
                    true => view.set_count("[*]"),
                    false => view.set_count(&cell::count_label(cell)),
                }
                let was = old.position(cell.id()).map(|at| &old.cells()[at]);
                let Some(was) =
                    was.filter(|was| was.kind() == Kind::Code && cell.kind() == Kind::Code)
                else {
                    continue;
                };
                let (was, now) = (was.outputs(), cell.outputs());
                let from = was.iter().zip(now).take_while(|(a, b)| a == b).count();
                if from < was.len().max(now.len()) {
                    view.outputs.show(from, &now[from..]);
                }
            }
        }
        self.find_changed();
    }

    /// Whether the notebook is on screen.
    pub fn on_screen(&self) -> bool {
        self.scroller.is_mapped()
    }

    /// Follow `source`'s caret in a viewer, as a note's preview follows its note's: the cell it
    /// is in selected and its line a quarter down the view, once it has moved since last time,
    /// so outputs coming in leave the view where it is read.
    pub fn show_place_of(self: &Rc<Self>, source: &Rc<NotebookTab>) {
        let Some(id) = source.selected.get() else {
            return;
        };
        let caret = source.view_of(Some(id)).and_then(|v| v.editor().cloned());
        let line = caret.map_or(0, |e| e.buffer.iter_at_mark(&e.buffer.get_insert()).line());
        if self.followed.replace(Some((id, line))) == Some((id, line)) {
            return;
        }
        self.select(id);
        if let Some((_, editor)) = self.editor(id) {
            let buffer = &editor.buffer;
            buffer.place_cursor(&buffer.iter_at_line(line).unwrap_or(buffer.start_iter()));
        }
        self.show(id, Some(0.25));
    }

    /// What a rendered cell's page reaches in the window.
    pub fn set_wiring(&self, wiring: Option<crate::preview::Wiring>) {
        self.wiring.replace(wiring);
    }

    /// A page for a rendered cell or output, in the Preview Font at the notebook's zoom, a link
    /// to a heading going to whichever cell has it; `None` before the window has wired the tab.
    fn inline_page(self: &Rc<Self>) -> Option<crate::preview::Preview> {
        let wiring = self.wiring.borrow().clone()?;
        let page = crate::preview::Preview::inline(&wiring);
        let (_, font) = self.fonts.borrow().clone();
        page.set_font(font.as_deref());
        page.set_zoom(self.zoom.get());
        let weak = Rc::downgrade(self);
        page.connect_anchor(move |anchor| {
            if let Some(tab) = weak.upgrade() {
                tab.goto_heading(anchor);
            }
        });
        Some(page)
    }

    /// What renders a markdown output while the notebook is read: on a page of its own, as wide
    /// as the output it stands in for.
    pub(super) fn output_render(self: &Rc<Self>) -> output::Render {
        let weak = Rc::downgrade(self);
        Rc::new(move |text: &str| {
            let tab = weak.upgrade()?;
            let page = tab.inline_page()?;
            page.set_margins(0, 0, 0, 0);
            page.render(&tab.key(), text);
            Some(page)
        })
    }

    /// Render markdown cell `view` on a page of its own, if the notebook is read and it has
    /// none: its source shows until the page has settled.
    pub(super) fn render_cell(self: &Rc<Self>, view: &Rc<CellView>) {
        if !self.renders(view.id) || view.has_rendered() {
            return;
        }
        let Some(page) = self.inline_page() else {
            return;
        };
        let (cell, tab) = (Rc::downgrade(view), Rc::downgrade(self));
        page.connect_settled(move || {
            let (Some(cell), Some(tab)) = (cell.upgrade(), tab.upgrade()) else {
                return;
            };
            // From an idle: the page is still settling, and its height asks for a layout.
            glib::idle_add_local_once(move || {
                cell.reveal_rendered();
                tab.find_changed();
            });
        });
        let source = {
            let model = self.model.borrow();
            let at = model.position(view.id);
            at.map(|at| model.cells()[at].source().to_string())
        };
        view.set_rendered(page, &self.key(), &source.unwrap_or_default());
    }

    /// Run `f` on each rendered cell's and output's page: what the window does to every preview.
    pub fn each_page(&self, f: &mut dyn FnMut(&crate::preview::Preview)) {
        for view in self.views.borrow().iter() {
            view.with_rendered(|page| f(page));
            view.outputs.each_page(f);
        }
    }

    /// The reading keys over the column while it holds the keyboard, a viewer's always and a
    /// presented notebook's over a rendered cell: Space and Page Down a page on, Shift+Space and
    /// Page Up one back, the arrows a step, Home and End to either end, as a note's preview reads.
    pub(super) fn wire_reading(self: &Rc<Self>) {
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| {
                let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
                let others =
                    gtk::accelerator_get_default_mod_mask() - gdk::ModifierType::SHIFT_MASK;
                if !tab.reading() || !tab.column.has_focus() || state.intersects(others) {
                    return glib::Propagation::Proceed;
                }
                let adjustment = tab.scroller.vadjustment();
                let (value, page) = (adjustment.value(), adjustment.page_increment());
                let step = adjustment.step_increment();
                let to = match key {
                    gdk::Key::space if shift => value - page,
                    gdk::Key::space | gdk::Key::Page_Down | gdk::Key::KP_Page_Down => value + page,
                    gdk::Key::Page_Up | gdk::Key::KP_Page_Up => value - page,
                    gdk::Key::Down | gdk::Key::KP_Down => value + step,
                    gdk::Key::Up | gdk::Key::KP_Up => value - step,
                    gdk::Key::Home | gdk::Key::KP_Home => adjustment.lower(),
                    gdk::Key::End | gdk::Key::KP_End => adjustment.upper(),
                    _ => return glib::Propagation::Proceed,
                };
                adjustment.set_value(to);
                glib::Propagation::Stop
            }
        ));
        self.column.add_controller(keys);
    }
}
