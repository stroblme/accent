//! Share scroll adjustments between the PDF view and diagram canvas.
//!
//! [`Adjustments`] follows adjustments weakly and configures their content bounds. Widgets
//! still declare the four `GtkScrollable` properties because GObject accesses them by name.

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

    /// Follow horizontal (`slot` 0) or vertical (1) `adjustment`, replacing its previous handler
    /// and requesting layout. Changes call `moved` on `widget`.
    ///
    /// Handlers hold widgets weakly: widgets retain adjustments after their scrolled window
    /// closes, so strong captures would keep canvases and their rendering resources alive.
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

/// Clamp adjustment upper bounds to at least page size, as GTK requires.
/// Content smaller than its viewport has no scroll and clamps to zero.
fn configure(adjustment: Option<gtk::Adjustment>, upper: f64, page: f64) {
    let Some(adjustment) = adjustment else {
        return;
    };
    let value = adjustment.value().min((upper - page).max(0.0));
    adjustment.configure(value, 0.0, upper.max(page), page * 0.1, page * 0.9, page);
}
