//! Diagram context menus, action keys and page-turning scroll overshoot.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use accent_drawio::CellId;
use adw::prelude::*;
use gtk::{gdk, gio, glib};

use super::geometry::{Overshoot, REST_MS};
use super::props::Clip;
use super::{DiagramTab, Tool};

impl DiagramTab {
    /// Select the right-clicked cell unless already selected, or clear selection on
    /// empty space, matching draw.io's `mxPopupMenuHandler`.
    pub(super) fn wire_menu(self: &Rc<Self>) {
        let secondary = gtk::GestureClick::builder()
            .button(gdk::BUTTON_SECONDARY)
            .build();
        secondary.connect_pressed(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            // Every entry edits, and a presented page is read only. The press gives the canvas
            // the keyboard, as any press on a PDF page does, so the keys act on what the menu did.
            move |_, _, x, y| {
                tab.view.grab_focus();
                if tab.presenting.get().is_none() {
                    tab.menu_at(x, y);
                }
            }
        ));
        self.view.add_controller(secondary);
    }

    /// The menu under the pointer, at widget `(x, y)`: the clipboard and Duplicate and Delete,
    /// Copy Shape for one shape and Copy Style for one cell, Group or Ungroup where they apply,
    /// the order, and Edit Label for one cell; over nothing selected, Paste alone. The one paste
    /// offered is for what the clipboard holds: Paste Shape for a size, Paste Style for a style,
    /// none for a position, and Paste for anything else. Window actions, so the palette lists
    /// them and they can be rebound.
    fn menu_at(self: &Rc<Self>, x: f64, y: f64) {
        match self.view.cell_at(x, y) {
            Some(cell) if !self.selection.borrow().contains(&cell) => self.select(vec![cell]),
            Some(_) => {}
            None => self.select(Vec::new()),
        }
        let ids = self.selection();
        let held = Clip::held(&self.view.clipboard());
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
            if held.is_none() {
                section(&["win.diagram-paste"]);
            }
        } else {
            let mut clipboard = vec!["win.diagram-cut", "win.diagram-copy"];
            if held.is_none() {
                clipboard.push("win.diagram-paste");
            }
            clipboard.extend(["win.diagram-duplicate", "win.diagram-delete"]);
            section(&clipboard);
            let (groups, shape) = {
                let editor = self.editor.borrow();
                let page = editor.page(self.page_index.get());
                let group =
                    |id: &CellId| page.as_ref().is_ok_and(|p| p.children(id).next().is_some());
                // The one shape whose size Position and Size show, and whether it is pinned.
                let shape = match ids.as_slice() {
                    [id] => (page.as_ref().ok().and_then(|p| p.cell(id)))
                        .filter(|c| c.vertex && c.geometry.as_ref().is_some_and(|g| !g.relative))
                        .map(|_| self.view.sheet().is_some_and(|s| s.is_pinned(id))),
                    _ => None,
                };
                (ids.iter().any(group), shape)
            };
            let mut looks = Vec::new();
            if let Some(pinned) = shape {
                looks.push("win.diagram-copy-shape");
                if !pinned && held == Some(Clip::Size) {
                    looks.push("win.diagram-paste-shape");
                }
            }
            if ids.len() == 1 {
                looks.push("win.diagram-copy-style");
            }
            if held == Some(Clip::Style) {
                looks.push("win.diagram-paste-style");
            }
            section(&looks);
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
        // Over nothing selected with a position, a size or a style held, there is nothing to offer.
        if menu.n_items() == 0 {
            return;
        }
        // Parented to the tab's box, not the canvas, which allocates itself: a popover on it
        // would never be presented again (DESIGN.md, States).
        let Some(host) = self.overlay.parent().and_downcast::<gtk::Box>() else {
            return;
        };
        let at = gtk::graphene::Point::new(x as f32, y as f32);
        let at = self.view.compute_point(&host, &at).unwrap_or(at);
        let anchor = gdk::Rectangle::new(at.x() as i32, at.y() as i32, 1, 1);
        crate::widgets::popup_menu(&host, &menu, Some(anchor));
    }

    /// Route canvas keys to window actions. Undo, Select All and Delete are local
    /// handlers so they act on the focused widget rather than global accelerators.
    pub(super) fn wire_keys(self: &Rc<Self>) {
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, state| {
                let ctrl = state.contains(crate::platform::PRIMARY_MODIFIER);
                if !ctrl
                    && state.intersects(
                        gdk::ModifierType::CONTROL_MASK
                            | gdk::ModifierType::META_MASK
                            | gdk::ModifierType::SUPER_MASK,
                    )
                {
                    return glib::Propagation::Proceed;
                }
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
                    // `n`, `p`, `Home` and `End` read a presented PDF, and a presented diagram so.
                    gdk::Key::n if presenting && !ctrl => tab.run("win.diagram-next-page"),
                    gdk::Key::p if presenting && !ctrl => tab.run("win.diagram-previous-page"),
                    gdk::Key::Home if presenting && !ctrl => tab.show_page(0),
                    gdk::Key::End if presenting && !ctrl => {
                        tab.show_page(tab.page_count().saturating_sub(1))
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

    /// A plain wheel pushed on past the top or bottom of the page turns it ([`Overshoot`]): a
    /// page being edited is pulled off its edge first, and springs back once the push rests for
    /// [`REST_MS`]. Ahead of the scrolled window, which scrolls whatever this passes on.
    pub(super) fn wire_wheel(self: &Rc<Self>) {
        let overshoot = Rc::new(Cell::new(Overshoot::default()));
        let resting: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        wheel.connect_scroll(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[strong]
            overshoot,
            #[strong]
            resting,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |wheel, dx, dy| {
                // Ctrl zooms (`zoom::zoom_on_wheel`), and Shift and a sideways swipe scroll across.
                let held = crate::platform::PRIMARY_MODIFIER | gdk::ModifierType::SHIFT_MASK;
                if wheel.current_event_state().intersects(held) || dx.abs() >= dy.abs() {
                    return glib::Propagation::Proceed;
                }
                let room = tab.view.has_room(dy > 0.0);
                let ahead = tab.next_page(dy > 0.0).is_some();
                let swipe = wheel.unit() == gdk::ScrollUnit::Surface;
                // A presented page is a slide, turned by a notch as a presented PDF's is.
                let spring = tab.presenting.get().is_none();
                let mut push = overshoot.get();
                let turn = push.scroll(dy, room, swipe, spring);
                overshoot.set(push);
                match turn {
                    Some(forward) => {
                        tab.view.set_pull(0.0);
                        tab.turn_page(forward);
                    }
                    // Pulled only towards a page to turn to, and let go at once by a scroll.
                    None if spring => tab.view.set_pull(match ahead {
                        true => push.pull(swipe),
                        false => 0.0,
                    }),
                    None => {}
                }
                if spring {
                    tab.rest_after(&overshoot, &resting);
                }
                // At the edge only the margin is left for the scrolled window to scroll, and a
                // swipe it saw begin would be its own to the end (its `smooth_scroll`), never
                // reaching here again: the push is kept while there is a page to turn to.
                match room || !ahead {
                    true => glib::Propagation::Proceed,
                    false => glib::Propagation::Stop,
                }
            }
        ));
        // The fingers left the touchpad: the page springs back, and the next swipe may turn it.
        wheel.connect_scroll_end(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_| {
                overshoot.set(Overshoot::default());
                if let Some(rest) = resting.take() {
                    rest.remove();
                }
                tab.view.release_pull();
            }
        ));
        self.view.add_controller(wheel);
    }

    /// Let the push go once it has rested for [`REST_MS`], each scroll putting that off: the page
    /// springs back, and after a turn the next push may turn again.
    fn rest_after(
        self: &Rc<Self>,
        overshoot: &Rc<Cell<Overshoot>>,
        resting: &Rc<RefCell<Option<glib::SourceId>>>,
    ) {
        if let Some(rest) = resting.take() {
            rest.remove();
        }
        let (tab, overshoot, kept) = (Rc::downgrade(self), overshoot.clone(), resting.clone());
        let rest = glib::timeout_add_local_once(Duration::from_millis(REST_MS), move || {
            // Gone by now, so never removed again.
            kept.take();
            let mut push = overshoot.get();
            push.rest();
            overshoot.set(push);
            if let Some(tab) = tab.upgrade() {
                tab.view.release_pull();
            }
        });
        resting.replace(Some(rest));
    }

    /// Fire a window action from the canvas's own keys (the PDF tab's `run`).
    fn run(&self, action: &str) {
        let _ = self.view.activate_action(action, None);
    }
}
