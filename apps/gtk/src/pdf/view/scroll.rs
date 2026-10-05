//! Where the view is in the document: the scroll offset as a page and a point on it, going to a
//! page or a rectangle of one, and the layout kept across a resize or a zoom.

use adw::prelude::*;
use gtk::gdk;
use gtk::subclass::prelude::*;

use super::PdfView;
use crate::pdf::geometry::{
    Anchor, PdfZoom, anchor_at, clamp_layout, fit_scale, layout, offset_of, page_at, resume_at,
};

impl PdfView {
    pub(super) fn zoom_around(&self, zoom: PdfZoom, at: Option<(f64, f64)>) {
        let Some((x, y)) = at else {
            return self.set_zoom(zoom);
        };
        let (ox, oy) = self.scroll_offset();
        let (old_w, old_h) = {
            let layout = self.imp().layout.borrow();
            (layout.width, layout.height)
        };
        self.imp().zoom.set(zoom);
        self.relayout();
        let layout = self.imp().layout.borrow().clone();
        // The same fraction of the content stays under the pointer, which is what makes zooming
        // feel like moving the page rather than moving the window.
        let (hadj, vadj) = (self.hadjustment(), self.vadjustment());
        if let Some(hadj) = hadj {
            hadj.set_value(crate::zoom::zoomed_scroll(
                x,
                ox,
                old_w.into(),
                layout.width.into(),
            ));
        }
        if let Some(vadj) = vadj {
            vadj.set_value(crate::zoom::zoomed_scroll(
                y,
                oy,
                old_h.into(),
                layout.height.into(),
            ));
        }
        self.zoomed();
    }

    /// Where the reader is now.
    pub fn anchor(&self) -> Anchor {
        let (x, y) = self.scroll_offset();
        anchor_at(&self.imp().layout.borrow(), x, y)
    }

    /// Put the reader back where `anchor` says, or at the top of its page under
    /// [`PdfZoom::FitPage`]. See [`resume_at`].
    pub fn scroll_to(&self, anchor: Anchor) {
        let anchor = resume_at(self.imp().zoom.get(), anchor);
        let at = offset_of(&self.imp().layout.borrow(), anchor);
        let Some((x, y)) = at else {
            return;
        };
        if let Some(hadj) = self.hadjustment() {
            hadj.set_value(x);
        }
        if let Some(vadj) = self.vadjustment() {
            vadj.set_value(y);
        }
    }

    /// Scroll so `page` starts at the top of the viewport, `top` points down it if given.
    pub fn goto_page(&self, page: usize, top: Option<f32>) {
        let scale = self.imp().layout.borrow().scale;
        let v = top.map_or(0.0, |top| top * scale);
        let layout = self.imp().layout.borrow().clone();
        let Some(rect) = layout.pages.get(page) else {
            return;
        };
        if let Some(vadj) = self.vadjustment() {
            vadj.set_value(f64::from(rect.y + v));
        }
    }

    /// Scroll one step up or down, which is what the arrow keys ask for.
    ///
    /// `GtkScrolledWindow` binds a step to `Ctrl+Up` and `Ctrl+Down` and the bare arrows to
    /// nothing, so this is the same move under the key a reader reaches for. The adjustment
    /// clamps its own value, so the two ends of the document need no case here.
    pub fn scroll_step(&self, down: bool) {
        let Some(vadj) = self.vadjustment() else {
            return;
        };
        let step = vadj.step_increment();
        vadj.set_value(vadj.value() + if down { step } else { -step });
    }

    /// Bring a rectangle of a page into view, for a search match.
    pub fn reveal(&self, page: usize, rect: accent_core::pdf::Rect) {
        let layout = self.imp().layout.borrow().clone();
        let Some(page_rect) = layout.pages.get(page) else {
            return;
        };
        let Some(vadj) = self.vadjustment() else {
            return;
        };
        let top = f64::from(page_rect.y + rect.top * layout.scale);
        let bottom = f64::from(page_rect.y + rect.bottom * layout.scale);
        let (value, size) = (vadj.value(), vadj.page_size());
        // Only if it is not already comfortably on screen, so stepping through matches on one
        // page does not jerk the view for each of them.
        if top < value || bottom > value + size {
            vadj.set_value(top - size / 3.0);
        }
    }

    /// The page under the middle of the viewport: what "page 4 of 12" means.
    pub fn current_page(&self) -> usize {
        let (_, y) = self.scroll_offset();
        let middle = y + self.vadjustment().map_or(0.0, |a| a.page_size()) / 2.0;
        page_at(&self.imp().layout.borrow(), middle)
    }

    /// The pages at least partly on screen, first to last.
    pub fn visible_pages(&self) -> std::ops::RangeInclusive<usize> {
        let (_, y) = self.scroll_offset();
        let height = self.vadjustment().map_or(0.0, |a| a.page_size());
        let layout = self.imp().layout.borrow();
        page_at(&layout, y)..=page_at(&layout, y + height)
    }

    /// Turn a widget coordinate into a page and a point on it.
    pub fn page_point(&self, x: f64, y: f64) -> Option<(usize, f32, f32)> {
        let (cx, cy) = self.content_at(x, y);
        let layout = self.imp().layout.borrow();
        let (page, _) = layout
            .pages
            .iter()
            .enumerate()
            .find(|(_, r)| cy >= r.y && cy <= r.y + r.h && cx >= r.x && cx <= r.x + r.w)?;
        let (x, y) = layout.to_page(page, cx, cy)?;
        Some((page, x, y))
    }

    /// A rectangle of a page, in its own points, in widget coordinates: the inverse of
    /// [`Self::page_point`].
    pub fn widget_rect(&self, page: usize, r: &accent_core::pdf::Rect) -> Option<gdk::Rectangle> {
        let layout = self.imp().layout.borrow();
        let rect = layout.rect_of(layout.pages.get(page)?, r);
        let (ox, oy) = self.scroll_offset();
        Some(gdk::Rectangle::new(
            (f64::from(rect.x()) - ox).floor() as i32,
            (f64::from(rect.y()) - oy).floor() as i32,
            rect.width().ceil() as i32,
            rect.height().ceil() as i32,
        ))
    }

    pub(super) fn content_at(&self, x: f64, y: f64) -> (f32, f32) {
        let (ox, oy) = self.scroll_offset();
        ((x + ox) as f32, (y + oy) as f32)
    }

    pub(super) fn scroll_offset(&self) -> (f64, f64) {
        self.imp().scroll.scroll()
    }

    fn hadjustment(&self) -> Option<gtk::Adjustment> {
        self.imp().scroll.h()
    }

    fn vadjustment(&self) -> Option<gtk::Adjustment> {
        self.imp().scroll.v()
    }

    /// Recompute the layout for the current size and zoom, and tell the scrollbars.
    ///
    /// The reading position is kept across the recompute, because it is the one thing the raw
    /// scroll offset cannot carry: a resize or a zoom moves every page, so the same number of
    /// pixels down the content is a different place in the document.
    pub(super) fn relayout(&self) {
        let (w, h) = (self.width(), self.height());
        if w <= 1 || h <= 1 {
            return;
        }
        let sizes = self.imp().sizes.borrow().clone();
        if sizes.is_empty() {
            return;
        }
        // Nothing to keep before the first layout: the offset is zero and page one is where the
        // reader is anyway.
        let anchor = (!self.imp().layout.borrow().pages.is_empty()).then(|| self.anchor());
        let scale = clamp_layout(fit_scale(&sizes, self.imp().zoom.get(), w as f32, h as f32));
        // Tiles waiting for a re-render at the scale being left are of a page nothing will paint
        // again; the keys are the old scale's and would sit in the set for the life of the tab.
        if self.imp().layout.borrow().scale != scale {
            self.imp().stale_tiles.borrow_mut().clear();
        }
        let layout = layout(&sizes, scale, w as f32);
        let (width, height) = (f64::from(layout.width), f64::from(layout.height));
        *self.imp().layout.borrow_mut() = layout;
        let viewport = (f64::from(w), f64::from(h));
        self.imp().scroll.configure((width, height), viewport);
        if let Some(anchor) = anchor {
            self.scroll_to(anchor);
        }
        self.queue_draw();
    }
}
