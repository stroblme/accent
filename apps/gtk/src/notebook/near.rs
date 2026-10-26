//! The cells near the view: their editors and outputs built as they near it, and what shows kept
//! still as the cells above it change height.

use std::rc::Rc;

use accent_core::notebook::CellId;
use adw::prelude::*;
use gtk::{gdk, glib};

use super::NotebookTab;
use super::cell::CellView;

impl NotebookTab {
    /// After each layout of the window, what shows stays put as cells change height
    /// ([`keep_anchor`](Self::keep_anchor)) and editors are built as their cells near the view
    /// ([`build_near`](Self::build_near)).
    pub(super) fn wire_building(self: &Rc<Self>) {
        let hook = |tab: &Rc<NotebookTab>| {
            let Some(clock) = tab.scroller.frame_clock() else {
                return;
            };
            // After the window's own handler, whose allocations it reads.
            let weak = Rc::downgrade(tab);
            let id = clock.connect_local("layout", true, move |values| {
                let clock = values[0].get::<gdk::FrameClock>().ok()?;
                weak.upgrade()?.laid_out(&clock);
                None
            });
            tab.layout_hook.replace(Some((clock, id)));
        };
        // The first tab of a pane is realised as it is added.
        if self.scroller.is_realized() {
            hook(self);
        }
        self.scroller.connect_realize(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_| hook(&tab)
        ));
        self.scroller.connect_unrealize(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_| {
                if let Some((clock, id)) = tab.layout_hook.take() {
                    clock.disconnect(id);
                }
            }
        ));
    }

    /// The window was laid out. What it lays out again before the frame is drawn, after a scroll
    /// or an editor built here, comes back through here.
    pub(super) fn laid_out(self: &Rc<Self>, clock: &gdk::FrameClock) {
        if !self.scroller.is_mapped() {
            return;
        }
        let adjustment = self.scroller.vadjustment();
        // Scrolled since by more than the notebook: the first cell in view holds it again.
        let value = adjustment.value();
        if self.shown.get().is_some_and(|(_, at)| at != value) {
            self.shown.set(None);
        }
        self.keep_anchor();
        self.show_pending();
        self.build_near(clock);
        // Every pass, not once a frame: GTK lays out again within the frame, and a view built in
        // one pass first wraps at its width in the next.
        for view in self
            .near
            .borrow()
            .iter()
            .filter(|v| v.root.parent().is_some())
        {
            view.refit();
        }
        self.refit_diagnostics(self.refit_after_layout.replace(false));
        // A cell shown far from where the view was stays where it landed while the cells around
        // it, in view too, are built and take their own heights.
        let shown = self.shown.get().map(|(id, _)| (id, adjustment.value()));
        self.shown.set(shown);
        let held = shown.and_then(|(id, _)| self.top_of(id));
        self.anchor.set(held.or_else(|| self.top_cell()));
    }

    /// Build the editors and the outputs of the cells within a screen of what shows, once a frame
    /// after the window is laid out, so the frame lays them out again and draws no cell scrolled
    /// to without them.
    pub(super) fn build_near(self: &Rc<Self>, clock: &gdk::FrameClock) {
        let frame = clock.frame_counter();
        if self.built_at.replace(frame) == frame {
            return;
        }
        let near = self.near();
        for view in &near {
            if view.editor().is_none() {
                self.build_editor(view);
            }
        }
        {
            let model = self.model.borrow();
            for view in &near {
                if let Some(at) = model.position(view.id) {
                    view.outputs.build(&model.cells()[at]);
                }
            }
        }
        let was = self.near.replace(near);
        self.fade_near(&was);
    }

    /// The views of the cells within a screen above and below what shows, in order.
    pub(super) fn near(&self) -> Vec<Rc<CellView>> {
        let adjustment = self.scroller.vadjustment();
        let page = adjustment.page_size();
        let Some(content) = self.content().filter(|_| page > 0.0) else {
            return Vec::new();
        };
        let (top, bottom) = (adjustment.value() - page, adjustment.value() + 2.0 * page);
        let mut near = Vec::new();
        for view in self.views.borrow().iter() {
            let Some(bounds) = view.root.compute_bounds(&content) else {
                continue;
            };
            if f64::from(bounds.y()) > bottom {
                break;
            }
            if f64::from(bounds.y() + bounds.height()) >= top {
                near.push(view.clone());
            }
        }
        near
    }

    /// Keep what shows where it is when the cells above it change height, an editor built in its
    /// placeholder's place or a picture decoded: the first cell in view (or the one shown) moved
    /// in the content since the last layout, so the view moves as far. Not at the top, where what
    /// is added above shows.
    pub(super) fn keep_anchor(&self) {
        let adjustment = self.scroller.vadjustment();
        let value = adjustment.value();
        let (Some((id, was)), Some(content)) = (self.anchor.get(), self.content()) else {
            return;
        };
        let now = self
            .view_of(Some(id))
            .and_then(|v| v.root.compute_bounds(&content));
        if let Some(now) = now.map(|b| f64::from(b.y()))
            && now != was
            && value > 0.0
        {
            adjustment.set_value(value + now - was);
        }
    }

    /// Cell `id`, and its top in the content.
    pub(super) fn top_of(&self, id: CellId) -> Option<(CellId, f64)> {
        let bounds = self
            .view_of(Some(id))?
            .root
            .compute_bounds(&self.content()?)?;
        Some((id, f64::from(bounds.y())))
    }

    /// The first cell reaching into the view, and its top in the content.
    pub(super) fn top_cell(&self) -> Option<(CellId, f64)> {
        let value = self.scroller.vadjustment().value();
        let content = self.content()?;
        self.views.borrow().iter().find_map(|v| {
            let bounds = v.root.compute_bounds(&content)?;
            let top = f64::from(bounds.y());
            (top + f64::from(bounds.height()) > value).then_some((v.id, top))
        })
    }
}
