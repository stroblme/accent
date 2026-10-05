//! What reaches a diagram tab from the canvas besides a gesture: the menu on a secondary click,
//! the keys, which fire the window's actions, and a wheel pushed on past the page's edge.

use std::cell::Cell;
use std::rc::Rc;

use accent_drawio::CellId;
use adw::prelude::*;
use gtk::{gdk, gio, glib};

use super::geometry::Overshoot;
use super::{DiagramTab, Tool};

impl DiagramTab {
    /// The canvas's own menu on a secondary click, as a PDF page has one: the cell under the
    /// pointer is selected first unless it is already, and empty page lets the selection go, as
    /// draw.io's `mxPopupMenuHandler` does.
    pub(super) fn wire_menu(self: &Rc<Self>) {
        let secondary = gtk::GestureClick::builder()
            .button(gdk::BUTTON_SECONDARY)
            .build();
        secondary.connect_pressed(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            // Every entry edits, and a presented page is read only.
            move |_, _, x, y| {
                if tab.presenting.get().is_none() {
                    tab.menu_at(x, y);
                }
            }
        ));
        self.view.add_controller(secondary);
    }

    /// The menu under the pointer, at widget `(x, y)`: the clipboard and Duplicate and Delete,
    /// Group or Ungroup where they apply, the order, and Edit Label for one cell; over nothing
    /// selected, Paste alone. Window actions, so the palette lists them and they can be rebound.
    fn menu_at(self: &Rc<Self>, x: f64, y: f64) {
        match self.view.cell_at(x, y) {
            Some(cell) if !self.selection.borrow().contains(&cell) => self.select(vec![cell]),
            Some(_) => {}
            None => self.select(Vec::new()),
        }
        let ids = self.selection();
        let menu = gio::Menu::new();
        let section = |actions: &[&str]| {
            let part = gio::Menu::new();
            for action in actions {
                part.append(Some(crate::actions::label_of(action)), Some(action));
            }
            if part.n_items() > 0 {
                menu.append_section(None, &part);
            }
        };
        if ids.is_empty() {
            section(&["win.diagram-paste"]);
        } else {
            section(&[
                "win.diagram-cut",
                "win.diagram-copy",
                "win.diagram-paste",
                "win.diagram-duplicate",
                "win.diagram-delete",
            ]);
            let groups = {
                let editor = self.editor.borrow();
                let page = editor.page(self.page_index.get());
                let group =
                    |id: &CellId| page.as_ref().is_ok_and(|p| p.children(id).next().is_some());
                ids.iter().any(group)
            };
            let mut grouping = Vec::new();
            if ids.len() > 1 {
                grouping.push("win.diagram-group");
            }
            if groups {
                grouping.push("win.diagram-ungroup");
            }
            section(&grouping);
            section(&["win.diagram-to-front", "win.diagram-to-back"]);
            if ids.len() == 1 {
                section(&["win.diagram-edit-label"]);
            }
        }
        // Parented to the tab's box, not the canvas, which allocates itself: a popover on it
        // would never be presented again (DESIGN.md, States).
        let Some(host) = self.page.child().downcast::<gtk::Box>().ok() else {
            return;
        };
        let at = gtk::graphene::Point::new(x as f32, y as f32);
        let at = self.view.compute_point(&host, &at).unwrap_or(at);
        let anchor = gdk::Rectangle::new(at.x() as i32, at.y() as i32, 1, 1);
        crate::widgets::popup_menu(&host, &menu, Some(anchor));
    }

    /// The canvas's keys. Undo, Select All and Delete belong to whatever has the keyboard, so
    /// they are the canvas's own and fire the window's actions rather than being accelerators
    /// (the PDF doctrine, `actions.rs`).
    pub(super) fn wire_keys(self: &Rc<Self>) {
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| {
                let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
                let alt = state.contains(gdk::ModifierType::ALT_MASK);
                let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
                let step = if shift { 10.0 } else { 1.0 };
                let presenting = tab.presenting.get().is_some();
                match key {
                    // A presented page is a slide, which Space, the arrows and the paging keys
                    // read on through, as they do a presented PDF, and nothing edits: a chord goes
                    // on to the window, and any other key that reaches the canvas does nothing,
                    // an arrow included, which would move the keyboard off the page.
                    gdk::Key::space if presenting && shift => tab.run("win.diagram-previous-page"),
                    gdk::Key::space | gdk::Key::Right | gdk::Key::Page_Down
                        if presenting && !ctrl =>
                    {
                        tab.run("win.diagram-next-page")
                    }
                    gdk::Key::Left | gdk::Key::Page_Up if presenting && !ctrl => {
                        tab.run("win.diagram-previous-page")
                    }
                    _ if presenting && (ctrl || alt) => return glib::Propagation::Proceed,
                    _ if presenting => {}
                    gdk::Key::z | gdk::Key::Z if ctrl && shift => tab.run("win.diagram-redo"),
                    gdk::Key::z if ctrl => tab.run("win.diagram-undo"),
                    gdk::Key::y if ctrl => tab.run("win.diagram-redo"),
                    gdk::Key::a if ctrl => tab.run("win.diagram-select-all"),
                    gdk::Key::c if ctrl => tab.run("win.diagram-copy"),
                    gdk::Key::x if ctrl => tab.run("win.diagram-cut"),
                    gdk::Key::v if ctrl => tab.run("win.diagram-paste"),
                    gdk::Key::Delete | gdk::Key::BackSpace if ctrl => {
                        tab.run("win.diagram-delete-all")
                    }
                    gdk::Key::Delete | gdk::Key::BackSpace => tab.run("win.diagram-delete"),
                    gdk::Key::Return | gdk::Key::KP_Enter if !ctrl => {
                        tab.run("win.diagram-edit-label")
                    }
                    gdk::Key::Page_Down if !ctrl => tab.run("win.diagram-next-page"),
                    gdk::Key::Page_Up if !ctrl => tab.run("win.diagram-previous-page"),
                    gdk::Key::Left if !ctrl => tab.nudge(-step, 0.0),
                    gdk::Key::Right if !ctrl => tab.nudge(step, 0.0),
                    gdk::Key::Up if !ctrl => tab.nudge(0.0, -step),
                    gdk::Key::Down if !ctrl => tab.nudge(0.0, step),
                    gdk::Key::space if !ctrl => tab.view.set_panning(true),
                    gdk::Key::Escape if tab.tool.get() != Tool::Select => {
                        tab.run("win.diagram-select")
                    }
                    gdk::Key::Escape if tab.has_selection() => tab.select(Vec::new()),
                    _ if !ctrl && !alt && tab.type_into_label(key.to_unicode()) => {}
                    _ => return glib::Propagation::Proceed,
                }
                glib::Propagation::Stop
            }
        ));
        keys.connect_key_released(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_, key, _, _| {
                if key == gdk::Key::space {
                    tab.view.set_panning(false);
                }
            }
        ));
        self.view.add_controller(keys);
        // A key held while the canvas lost the keyboard never comes up here.
        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_| tab.view.set_panning(false)
        ));
        self.view.add_controller(focus);
    }

    /// A plain wheel pushed on past the top or bottom of the page turns it ([`Overshoot`]). Ahead
    /// of the scrolled window, which scrolls whatever this passes on.
    pub(super) fn wire_wheel(self: &Rc<Self>) {
        let overshoot = Rc::new(Cell::new(Overshoot::default()));
        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        wheel.connect_scroll(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[strong]
            overshoot,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |wheel, dx, dy| {
                // Ctrl zooms (`zoom::zoom_on_wheel`), and Shift and a sideways swipe scroll across.
                let held = gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK;
                if wheel.current_event_state().intersects(held) || dx.abs() >= dy.abs() {
                    return glib::Propagation::Proceed;
                }
                let room = tab.view.has_room(dy > 0.0);
                let ahead = tab.next_page(dy > 0.0).is_some();
                let swipe = wheel.unit() == gdk::ScrollUnit::Surface;
                let mut push = overshoot.get();
                if let Some(forward) = push.scroll(dy, room, swipe) {
                    tab.turn_page(forward);
                }
                overshoot.set(push);
                // At the edge only the margin is left for the scrolled window to scroll, and a
                // swipe it saw begin would be its own to the end (its `smooth_scroll`), never
                // reaching here again: the push is kept while there is a page to turn to.
                match room || !ahead {
                    true => glib::Propagation::Proceed,
                    false => glib::Propagation::Stop,
                }
            }
        ));
        // The fingers left the touchpad: the next swipe may turn the page again.
        wheel.connect_scroll_end(move |_| overshoot.set(Overshoot::default()));
        self.view.add_controller(wheel);
    }

    /// Fire a window action from the canvas's own keys (the PDF tab's `run`).
    fn run(&self, action: &str) {
        let _ = self.view.activate_action(action, None);
    }
}
