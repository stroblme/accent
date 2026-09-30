//! The part of `GtkScrollable` a canvas that scrolls itself shares — the PDF view and the diagram
//! canvas: the two adjustments a scrolled window hands it, followed weakly, and pointed at the
//! content's size. Each widget still declares the interface's four properties (GObject reads
//! them by name), its adjustments through [`Adjustments`].

use std::cell::RefCell;

use gtk::glib;
use gtk::prelude::*;

/// The horizontal and the vertical adjustment, and the handler on each.
#[derive(Default)]
pub struct Adjustments {
    slots: [RefCell<Option<gtk::Adjustment>>; 2],
    handlers: [RefCell<Option<glib::SignalHandlerId>>; 2],
}

impl Adjustments {
    pub fn h(&self) -> Option<gtk::Adjustment> {
        self.slots[0].borrow().clone()
    }

    pub fn v(&self) -> Option<gtk::Adjustment> {
        self.slots[1].borrow().clone()
    }

    /// Follow `adjustment` as the horizontal one (`slot` 0) or the vertical one (1): `moved`
    /// runs on `widget` whenever it moves, the handler on the one it replaces goes, and the
    /// widget is laid out again. The handler holds the widget weakly: the widget holds the
    /// adjustment, and a scrolled window going away leaves its child's adjustments set, so a
    /// strong one kept every closed canvas alive — a PDF's rendered tiles, a diagram's WebKit
    /// process for its formulas.
    pub fn adopt<W: IsA<gtk::Widget>>(
        &self,
        widget: &W,
        slot: usize,
        adjustment: Option<gtk::Adjustment>,
        moved: impl Fn(&W) + 'static,
    ) {
        let old = self.slots[slot].replace(adjustment.clone());
        if let (Some(old), Some(id)) = (old, self.handlers[slot].take()) {
            old.disconnect(id);
        }
        if let Some(adjustment) = adjustment {
            let weak = widget.downgrade();
            let id = adjustment.connect_value_changed(move |_| {
                if let Some(widget) = weak.upgrade() {
                    moved(&widget);
                }
            });
            self.handlers[slot].replace(Some(id));
        }
        widget.queue_allocate();
    }

    /// Where the content is scrolled to.
    pub fn scroll(&self) -> (f64, f64) {
        let value = |a: Option<gtk::Adjustment>| a.map_or(0.0, |a| a.value());
        (value(self.h()), value(self.v()))
    }

    pub fn set_scroll(&self, (x, y): (f64, f64)) {
        if let Some(a) = self.h() {
            a.set_value(x);
        }
        if let Some(a) = self.v() {
            a.set_value(y);
        }
    }

    /// Point both at content of `size` shown in a `viewport`, keeping where they are scrolled
    /// to as far as the new size allows.
    pub fn configure(&self, size: (f64, f64), viewport: (f64, f64)) {
        configure(self.h(), size.0, viewport.0);
        configure(self.v(), size.1, viewport.1);
    }
}

/// The upper bound is never below the page size, which GTK asserts on and which a document
/// smaller than the window otherwise breaks — an A4 sketch in a split pane, or any small page in
/// a large one. There is nothing to scroll in that case either way: the value clamps to zero.
fn configure(adjustment: Option<gtk::Adjustment>, upper: f64, page: f64) {
    let Some(adjustment) = adjustment else {
        return;
    };
    let value = adjustment.value().min((upper - page).max(0.0));
    adjustment.configure(value, 0.0, upper.max(page), page * 0.1, page * 0.9, page);
}
