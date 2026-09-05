//! Side-by-side panes: one `AdwTabBar` and one `AdwTabView` each, nested in `GtkPaned`s.
//!
//! There is no model of the layout beside the widget tree; the widget tree *is* the tree. A pane's
//! parent is either the `AdwBin` that holds the whole arrangement or a `GtkPaned`, so a split is
//! "replace the pane in its parent with a paned holding the pane and its new neighbour", and a
//! close is the same swap backwards. Nothing else has to be kept in step.
//!
//! Moving a tab between panes needs no code of ours: libadwaita's tab drag carries the
//! `AdwTabPage` itself, every `AdwTabBox` and every `AdwTabView` already accepts that type, and
//! `AdwTabView:is-transferring-page` says when one is in flight. What is added here is the drop
//! *zones*: an edge of a pane means "split", which libadwaita has no notion of.

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::rc::Rc;

/// The class `main::install_chrome_css` paints the drop hint with.
const ZONE: &str = "accent-drop-zone";
/// How much of a pane's width or height each edge zone claims. A quarter is enough to aim at
/// without swallowing the middle, which is the far commoner drop.
const EDGE: f64 = 0.25;

/// Which side of a pane a new pane goes on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Left,
    Right,
    Up,
    Down,
}

impl Side {
    /// The suffix of the `win.split-*` action that asks for this side.
    pub fn action(self) -> &'static str {
        match self {
            Side::Left => "left",
            Side::Right => "right",
            Side::Up => "up",
            Side::Down => "down",
        }
    }
}

/// Where in a pane a drop landed: an edge splits it, the middle drops into it as it is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Zone {
    Here,
    Split(Side),
}

/// The zone a pointer at `(x, y)` is in, for a pane of `w` by `h`.
///
/// The nearest edge wins, so the corners belong to whichever side the pointer is closer to
/// rather than to neither.
pub fn zone(x: f64, y: f64, w: f64, h: f64) -> Zone {
    if w <= 0.0 || h <= 0.0 {
        return Zone::Here;
    }
    let edges = [
        (x / w, Side::Left),
        ((w - x) / w, Side::Right),
        (y / h, Side::Up),
        ((h - y) / h, Side::Down),
    ];
    let nearest = edges
        .into_iter()
        .reduce(|best, edge| if edge.0 < best.0 { edge } else { best });
    match nearest {
        Some((share, side)) if share < EDGE => Zone::Split(side),
        _ => Zone::Here,
    }
}

/// How a split arranges the two panes: the paned's orientation, and whether the new pane takes
/// the start child.
pub fn arrange(side: Side) -> (gtk::Orientation, bool) {
    match side {
        Side::Left => (gtk::Orientation::Horizontal, true),
        Side::Right => (gtk::Orientation::Horizontal, false),
        Side::Up => (gtk::Orientation::Vertical, true),
        Side::Down => (gtk::Orientation::Vertical, false),
    }
}

/// One pane: its tab bar, its tab view, and the sheet that shows where a drop would land.
pub struct Pane {
    /// Bar above, document below. This is what the paneds hold.
    column: gtk::Box,
    pub bar: adw::TabBar,
    pub tabs: adw::TabView,
    overlay: gtk::Overlay,
    /// Covers the whole document while a drag is in flight, and carries [`Pane::drop`]. It is an
    /// overlay child, so it is picked before the tab view, which has a tab drop target of its own
    /// that would otherwise take every drop as a plain "move it here".
    hint: gtk::Box,
    /// The lit rectangle inside `hint`. A child rather than the hint itself, so highlighting a
    /// zone never moves the widget the pointer coordinates are measured against.
    shade: gtk::Box,
    pub drop: gtk::DropTarget,
}

impl Pane {
    pub fn new(menu: &gio::Menu) -> Rc<Pane> {
        let tabs = adw::TabView::builder().hexpand(true).vexpand(true).build();
        tabs.set_menu_model(Some(menu));
        let bar = adw::TabBar::builder().view(&tabs).build();
        // The bar used to be one of the toolbar view's top bars, which draw flat on their own.
        // Inside the document column it needs `.inline` to stop painting a header-bar background
        // over the one flat surface (DESIGN.md, Colour).
        bar.add_css_class("inline");
        bar.add_css_class("chrome-fade");

        let shade = gtk::Box::builder().hexpand(true).vexpand(true).build();
        shade.set_visible(false);
        let hint = gtk::Box::new(gtk::Orientation::Vertical, 0);
        hint.append(&shade);
        // Mapped from the start and merely untargetable until a drag, rather than shown when one
        // begins. A widget that appears mid-drag has to be mapped and allocated before GTK can
        // pick it, and on Wayland that never happened: the drop zones took no drags at all, while
        // X11 picked the sheet on the next motion and worked. `can-target` is a flag on a widget
        // that is already laid out, so there is nothing to wait for. It draws nothing while it is
        // off, and `gtk_widget_pick` skips its children too, so clicks still reach the editor.
        hint.set_can_target(false);
        let drop = gtk::DropTarget::new(
            glib::Type::INVALID,
            gdk::DragAction::MOVE | gdk::DragAction::COPY,
        );
        drop.set_types(&[adw::TabPage::static_type(), String::static_type()]);
        hint.add_controller(drop.clone());

        let overlay = gtk::Overlay::builder().child(&tabs).build();
        overlay.add_overlay(&hint);

        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.append(&bar);
        column.append(&overlay);

        Rc::new(Pane {
            column,
            bar,
            tabs,
            overlay,
            hint,
            shade,
            drop,
        })
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.column.upcast_ref()
    }

    /// The document area, whose size the drop zones are measured in.
    pub fn size(&self) -> (f64, f64) {
        (
            f64::from(self.overlay.width()),
            f64::from(self.overlay.height()),
        )
    }

    /// Whether `page` is one of this pane's.
    pub fn has(&self, page: &adw::TabPage) -> bool {
        let pages = self.tabs.pages();
        (0..pages.n_items())
            .filter_map(|i| pages.item(i))
            .any(|item| item.downcast_ref::<adw::TabPage>() == Some(page))
    }

    /// Take the drop sheet in or out of the picture. It stays mapped either way and only stops
    /// being targetable, because a widget mapped mid-drag is not picked on every backend; while
    /// it is off it draws nothing and every click reaches the editor underneath.
    pub fn set_drop_active(&self, on: bool) {
        self.hint.set_can_target(on);
        if !on {
            self.show_zone(None);
        }
    }

    /// Light up the part of the pane a drop would land in, or nothing.
    pub fn show_zone(&self, zone: Option<Zone>) {
        let Some(zone) = zone else {
            self.shade.set_visible(false);
            return;
        };
        let (w, h) = (self.overlay.width(), self.overlay.height());
        let (start, end, top, bottom) = match zone {
            Zone::Here => (0, 0, 0, 0),
            Zone::Split(Side::Left) => (0, w / 2, 0, 0),
            Zone::Split(Side::Right) => (w / 2, 0, 0, 0),
            Zone::Split(Side::Up) => (0, 0, 0, h / 2),
            Zone::Split(Side::Down) => (0, 0, h / 2, 0),
        };
        self.shade.set_margin_start(start);
        self.shade.set_margin_end(end);
        self.shade.set_margin_top(top);
        self.shade.set_margin_bottom(bottom);
        self.shade.add_css_class(ZONE);
        self.shade.set_visible(true);
    }
}

/// Put `new` beside `pane` on `side`, by replacing `pane` in its parent with a paned holding
/// both. The handle starts at the middle of what the old pane had, which is the size it still
/// has at this point.
pub fn split(pane: &Pane, new: &Pane, side: Side) {
    let Some(parent) = pane.column.parent() else {
        return tracing::warn!("splitting a pane that is not in the window");
    };
    let (orientation, new_first) = arrange(side);
    let extent = match orientation {
        gtk::Orientation::Vertical => pane.column.height(),
        _ => pane.column.width(),
    };
    let paned = gtk::Paned::builder()
        .orientation(orientation)
        .resize_start_child(true)
        .resize_end_child(true)
        .shrink_start_child(false)
        .shrink_end_child(false)
        .position(extent / 2)
        .build();
    // The paned goes in first, which unparents the old column; then both columns go into it.
    replace(&parent, pane.widget(), paned.upcast_ref());
    let (first, second) = match new_first {
        true => (new.widget(), pane.widget()),
        false => (pane.widget(), new.widget()),
    };
    paned.set_start_child(Some(first));
    paned.set_end_child(Some(second));
}

/// Take `pane` out of the window, leaving its neighbour where the two of them were.
pub fn detach(pane: &Pane) {
    let Some(paned) = pane.column.parent().and_downcast::<gtk::Paned>() else {
        // The only pane left is held by the bin directly and has nothing to collapse into.
        return;
    };
    let sibling = match paned.start_child().as_ref() == Some(pane.widget()) {
        true => paned.end_child(),
        false => paned.start_child(),
    };
    let (Some(sibling), Some(grandparent)) = (sibling, paned.parent()) else {
        return;
    };
    paned.set_start_child(gtk::Widget::NONE);
    paned.set_end_child(gtk::Widget::NONE);
    replace(&grandparent, paned.upcast_ref(), &sibling);
}

/// Swap one child of a pane tree node for another. A node is either the bin at the root or a
/// paned; nothing else ever holds a pane column.
fn replace(parent: &gtk::Widget, old: &gtk::Widget, new: &gtk::Widget) {
    if let Some(bin) = parent.downcast_ref::<adw::Bin>() {
        bin.set_child(Some(new));
    } else if let Some(paned) = parent.downcast_ref::<gtk::Paned>() {
        match paned.start_child().as_ref() == Some(old) {
            true => paned.set_start_child(Some(new)),
            false => paned.set_end_child(Some(new)),
        }
    } else {
        tracing::warn!("a pane in a {}, which holds no panes", parent.type_());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_middle_of_a_pane_is_not_a_split() {
        assert_eq!(zone(200.0, 150.0, 400.0, 300.0), Zone::Here);
        // Just inside the quarter on every side.
        assert_eq!(zone(390.0, 150.0, 400.0, 300.0), Zone::Split(Side::Right));
        assert_eq!(zone(10.0, 150.0, 400.0, 300.0), Zone::Split(Side::Left));
        assert_eq!(zone(200.0, 10.0, 400.0, 300.0), Zone::Split(Side::Up));
        assert_eq!(zone(200.0, 290.0, 400.0, 300.0), Zone::Split(Side::Down));
    }

    #[test]
    fn a_corner_belongs_to_the_edge_it_is_nearest() {
        // 10 px from the left and 20 from the top, in a pane that is wide and short: 1/80 of the
        // width beats 1/10 of the height, so the left edge is the nearer one. Turn the pane on its
        // side and the answer turns with it.
        assert_eq!(zone(10.0, 20.0, 800.0, 200.0), Zone::Split(Side::Left));
        assert_eq!(zone(10.0, 20.0, 200.0, 800.0), Zone::Split(Side::Up));
    }

    #[test]
    fn an_unallocated_pane_has_no_edges() {
        assert_eq!(zone(0.0, 0.0, 0.0, 0.0), Zone::Here);
    }

    #[test]
    fn a_split_puts_the_new_pane_on_the_named_side() {
        assert_eq!(arrange(Side::Left), (gtk::Orientation::Horizontal, true));
        assert_eq!(arrange(Side::Right), (gtk::Orientation::Horizontal, false));
        assert_eq!(arrange(Side::Up), (gtk::Orientation::Vertical, true));
        assert_eq!(arrange(Side::Down), (gtk::Orientation::Vertical, false));
    }
}
