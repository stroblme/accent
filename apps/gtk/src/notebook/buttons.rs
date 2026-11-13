//! The cell buttons and the add strips, wired to the cell they serve and placed over the cell under
//! the pointer, else the selected one, once the event at hand is over.

use std::rc::Rc;

use accent_core::notebook::{CellId, Kind};
use adw::prelude::*;
use gtk::glib;

use super::NotebookTab;
use super::cell;

impl NotebookTab {
    /// The shared buttons act on the cell they are over, and the gaps' add strip after the cell
    /// it is under, selecting that cell first.
    pub(super) fn wire_tools(self: &Rc<Self>, add_code: &gtk::Button, add_markdown: &gtk::Button) {
        let act = |button: &gtk::Button, f: fn(&Rc<NotebookTab>)| {
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(tab) = weak.upgrade()
                    && let Some(id) = tab.tools.cell.get()
                {
                    tab.select(id);
                    f(&tab);
                }
            });
        };
        // Run Cell and Stop are the window's, whose vault the kernel runs where.
        act(&self.tools.run, |tab| {
            let _ = tab.scroller.activate_action("win.notebook-run-cell", None);
        });
        act(&self.tools.stop, |tab| {
            let _ = tab.scroller.activate_action("win.notebook-interrupt", None);
        });
        // Over the bar, which reaches past a short cell's rail, the pointer is still on its cell.
        let motion = gtk::EventControllerMotion::new();
        let over = |on: bool| {
            let weak = Rc::downgrade(self);
            move || {
                if let Some(tab) = weak.upgrade()
                    && let Some(id) = tab.tools.cell.get()
                {
                    tab.hover(id, on);
                }
            }
        };
        let (enter, leave) = (over(true), over(false));
        motion.connect_enter(move |_, _, _| enter());
        motion.connect_leave(move |_| leave());
        self.tools.bar.add_controller(motion);
        // A press on the buttons gives their cell the keyboard, as a press in it does, the view
        // staying where it is.
        let press = gtk::GestureClick::new();
        press.set_propagation_phase(gtk::PropagationPhase::Capture);
        press.connect_pressed(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_, _, _, _| {
                if let Some((_, editor)) = tab.tools.cell.get().and_then(|id| tab.editor(id)) {
                    editor.view.grab_focus();
                }
            }
        ));
        self.tools.bar.add_controller(press);
        act(&self.tools.up, |tab| tab.move_cell(false));
        act(&self.tools.down, |tab| tab.move_cell(true));
        act(&self.tools.delete, |tab| tab.delete());
        self.tools.more.connect_active_notify(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |more| {
                if more.is_active()
                    && let Some(id) = tab.tools.cell.get()
                {
                    tab.select(id);
                }
                // Closed, the menu hands the keyboard to its button once this is over; the cell
                // takes it back.
                if !more.is_active() {
                    let weak = Rc::downgrade(&tab);
                    glib::idle_add_local_once(move || {
                        if let Some(tab) = weak.upgrade()
                            && let Some((_, editor)) =
                                tab.selected.get().and_then(|id| tab.editor(id))
                        {
                            editor.view.grab_focus();
                        }
                    });
                }
            }
        ));
        let add = |button: &gtk::Button, kind: Kind| {
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(tab) = weak.upgrade()
                    && let Some(id) = tab.gap_cell.get()
                {
                    tab.select(id);
                    tab.insert(kind, true);
                }
            });
        };
        add(add_code, Kind::Code);
        add(add_markdown, Kind::Markdown);
    }

    /// The pointer came onto cell `id`'s card (`on`) or left it: the buttons go with it.
    pub(super) fn hover(self: &Rc<Self>, id: CellId, on: bool) {
        match on {
            true => self.hovered.set(Some(id)),
            false if self.hovered.get() == Some(id) => self.hovered.set(None),
            false => return,
        }
        self.place_soon();
    }

    /// The pointer came into cell `id`'s gap (`on`) or left it: the add strip goes with it.
    pub(super) fn hover_gap(self: &Rc<Self>, id: CellId, on: bool) {
        match on {
            true => self.gap_cell.set(Some(id)),
            false if self.gap_cell.get() == Some(id) => self.gap_cell.set(None),
            false => return,
        }
        self.place_soon();
    }

    /// Move the buttons and the add strip where the pointer and the selection say, once the
    /// event at hand is over: a placeholder leaving the tree under the pointer sends crossing
    /// events, and moving widgets about from inside those crashed GTK.
    pub(super) fn place_soon(self: &Rc<Self>) {
        if self.placing.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(tab) = weak.upgrade() {
                tab.placing.set(false);
                tab.place();
            }
        });
    }

    /// The buttons over the cell under the pointer, else the selected one, and the add strip in
    /// the gap under the pointer; neither while presented.
    pub(super) fn place(&self) {
        let shown = |id: Option<CellId>| match self.reading() {
            true => None,
            false => self.view_of(id),
        };
        let tools = shown(self.hovered.get().or(self.selected.get()));
        let turn = tools
            .as_ref()
            .map_or(cell::Turn::Free, |view| self.turn(view.id));
        self.tools.place(tools.as_deref(), turn);
        let gap = shown(self.gap_cell.get());
        let target = gap.as_ref().map(|v| v.gap.clone().upcast::<gtk::Widget>());
        if self.gap.parent() != target {
            if let Some(old) = self.gap.parent().and_downcast::<gtk::Overlay>() {
                old.remove_overlay(&self.gap);
            }
            if let Some(view) = gap {
                view.gap.add_overlay(&self.gap);
                self.gap_fade.fade_in();
            }
        }
    }
}
