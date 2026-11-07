//! The cells near the view: their editors and outputs built as they near it, and what shows kept
//! still as the cells above it change height, and through a zoom.

use std::rc::Rc;

use accent_core::notebook::CellId;
use adw::prelude::*;
use gtk::{gdk, glib, graphene};

use super::NotebookTab;
use super::cell::CellView;
use super::selection::line_in;

/// What a zoom keeps where it was in the view: what was at a point of it, the pointer's for a
/// Ctrl+scroll, the middle of the top for a chord, as the cells take their new heights.
pub(super) struct ZoomAnchor {
    /// How far down the view it is kept.
    at: f64,
    spot: Spot,
    /// The scroll the notebook left the view at, which a scroll of the reader's changes.
    value: f64,
}

/// What was at a point of the view.
enum Spot {
    /// A character of a text view, a cell's source or an output's text, and how far down the
    /// display line starting there the point was, in the line's heights.
    Text(gtk::TextView, i32, f64),
    /// Any other widget, a picture say, and how far down it the point was, in its heights.
    Part(gtk::Widget, f64),
}

impl Spot {
    /// Where the point is now in `content`, while it is still in the notebook.
    fn y_in(&self, content: &gtk::Widget) -> Option<f64> {
        match self {
            Spot::Text(view, offset, down) => {
                let iter = view.buffer().iter_at_offset(*offset);
                let (top, height) = line_in(view, &iter, content)?;
                Some(top + down * height)
            }
            Spot::Part(widget, down) => {
                let bounds = widget.compute_bounds(content)?;
                Some(f64::from(bounds.y()) + down * f64::from(bounds.height()))
            }
        }
    }
}

/// The start of the display line at buffer height `y` in `view`: [`gtk::TextView::line_at_y`]
/// gives the start of the paragraph, which may wrap over several.
fn display_line_at(view: &gtk::TextView, y: i32) -> gtk::TextIter {
    let (mut iter, _) = view.line_at_y(y);
    let mut next = iter;
    while view.forward_display_line(&mut next)
        && next.line() == iter.line()
        && view.iter_location(&next).y() <= y
    {
        iter = next;
    }
    iter
}

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
    /// is added above shows. After a zoom, what the zoom holds is kept instead ([`ZoomAnchor`]),
    /// which a cell growing around it would otherwise move.
    pub(super) fn keep_anchor(&self) {
        let adjustment = self.scroller.vadjustment();
        let value = adjustment.value();
        let Some(content) = self.content() else {
            return;
        };
        if self.keep_zoom_anchor(&content) {
            return;
        }
        let Some((id, was)) = self.anchor.get() else {
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

    /// What is at `at` in the view, else at the middle of its top, for the zoom about to be laid
    /// out to keep there ([`ZoomAnchor`]): a line of text is the character starting it, so a line
    /// wrapped again keeps that.
    pub(super) fn zoom_anchor_at(&self, at: Option<(f64, f64)>) -> Option<ZoomAnchor> {
        let content = self.content()?;
        let (x, at) = at.unwrap_or((f64::from(self.scroller.width()) / 2.0, 0.0));
        let value = self.scroller.vadjustment().value();
        let (x, y) = (x + self.scroller.hadjustment().value(), at + value);
        let picked = content.pick(x, y, gtk::PickFlags::DEFAULT)?;
        let spot = match picked.ancestor(gtk::TextView::static_type()) {
            Some(view) => {
                let view = view.downcast::<gtk::TextView>().ok()?;
                let point = graphene::Point::new(x as f32, y as f32);
                let point = content.compute_point(&view, &point)?;
                let (_, by) = view.window_to_buffer_coords(
                    gtk::TextWindowType::Widget,
                    point.x() as i32,
                    point.y() as i32,
                );
                let iter = display_line_at(&view, by);
                let (top, height) = line_in(&view, &iter, &content)?;
                Spot::Text(view, iter.offset(), (y - top) / height.max(1.0))
            }
            None => {
                let bounds = picked.compute_bounds(&content)?;
                let down = (y - f64::from(bounds.y())) / f64::from(bounds.height()).max(1.0);
                Spot::Part(picked, down)
            }
        };
        Some(ZoomAnchor { at, spot, value })
    }

    /// Keep what the last zoom holds at its height in the view, saying whether there is one: a
    /// scroll of the reader's since, or what it holds gone from the notebook, lets go of it.
    fn keep_zoom_anchor(&self, content: &gtk::Widget) -> bool {
        let adjustment = self.scroller.vadjustment();
        let kept = (self.zoom_anchor.borrow().as_ref())
            .filter(|anchor| anchor.value == adjustment.value())
            .and_then(|anchor| Some(anchor.spot.y_in(content)? - anchor.at));
        let Some(value) = kept else {
            self.zoom_anchor.replace(None);
            return false;
        };
        adjustment.set_value(value);
        if let Some(anchor) = self.zoom_anchor.borrow_mut().as_mut() {
            anchor.value = adjustment.value();
        }
        true
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
