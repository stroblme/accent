//! Sections one above the other in a sidebar pane, sharing its height across dividers the reader
//! drags, as VS Code's views do.

use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// A share of a divider's height, where the paned's own handlers can reach it.
type Share = Rc<Cell<Option<f64>>>;

/// A divider between two parts of the pane. While both sides are open it is held at an even
/// split of what is open, or at the share of its height the reader dragged it to, which it keeps
/// through the side beside it being shut and opened again.
struct Divider {
    paned: gtk::Paned,
    /// The start side's share where the reader last dragged the divider; `None` until they do,
    /// which keeps it at an even split however many sections come and go.
    share: Share,
    /// The share the divider is held at while both sides are open; `None` while one is shut.
    held: Share,
    /// Whether a placing is waiting for the frame's layout.
    placing: Rc<Cell<bool>>,
}

impl Divider {
    fn new(start: &impl IsA<gtk::Widget>, end: &impl IsA<gtk::Widget>) -> Divider {
        let paned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Vertical)
            .start_child(start)
            .end_child(end)
            .shrink_start_child(false)
            .shrink_end_child(false)
            .vexpand(true)
            .build();
        let (share, held): (Share, Share) = Default::default();
        // A drag is told from every other move by the class `paned::watch` puts on a handle
        // while it is held: the pane placing the divider, and GTK handing a resize out, are not
        // the reader's.
        paned.connect_position_notify({
            let (share, held) = (share.clone(), held.clone());
            move |paned| {
                let height = paned.height();
                if paned.has_css_class(crate::paned::DRAGGING) && held.get().is_some() && height > 0
                {
                    let dragged = f64::from(paned.position()) / f64::from(height);
                    share.set(Some(dragged));
                    held.set(Some(dragged));
                }
            }
        });
        Divider {
            paned,
            share,
            held,
            placing: Rc::default(),
        }
    }

    /// Lay the divider out for which of its sides are open: while both are, at the reader's
    /// share or else at `even`, which moves with the sections open; else against the shut side,
    /// `GtkPaned` sizing that side to its header when no position is set and only the other side
    /// takes the space.
    fn fit(&self, start: bool, end: bool, even: f64) {
        self.paned.set_resize_start_child(start);
        self.paned.set_resize_end_child(end || !start);
        let held = (start && end).then(|| self.share.get().unwrap_or(even));
        self.held.set(held);
        match held {
            Some(_) => self.place(),
            None => self.paned.set_position(-1),
        }
    }

    /// Place the divider during frame layout after the window allocates its new height. Section
    /// visibility may change that height in the same frame, so using the old allocation would
    /// preserve the wrong split. Un-realised panes are placed on map.
    fn place(&self) {
        let Some(clock) = self.paned.frame_clock() else {
            return;
        };
        if self.placing.replace(true) {
            return;
        }
        let handler = Rc::new(RefCell::new(None));
        let id = clock.connect_layout({
            let (paned, held, placing) = (
                self.paned.downgrade(),
                self.held.clone(),
                self.placing.clone(),
            );
            let handler = handler.clone();
            move |clock| {
                if let Some(id) = handler.take() {
                    clock.disconnect(id);
                }
                placing.set(false);
                if let (Some(paned), Some(share)) = (paned.upgrade(), held.get())
                    && paned.height() > 0
                {
                    paned.set_position(at(&paned, share));
                }
            }
        });
        *handler.borrow_mut() = Some(id);
        clock.request_phase(gtk::gdk::FrameClockPhase::LAYOUT);
    }

    /// Back to an even split: a double-click on the handle.
    fn reset(&self, even: f64) {
        self.share.set(None);
        if self.held.get().is_some() {
            self.held.set(Some(even));
            self.place();
        }
    }
}

/// `share` of the paned's height, in pixels.
fn at(paned: &gtk::Paned, share: f64) -> i32 {
    (share * f64::from(paned.height())).round() as i32
}

/// Sections one above the other, sharing the height across dividers the reader drags: a
/// vertical `GtkPaned` per divider, each holding one section over the paned of those below.
pub(super) struct Sections {
    /// The top paned, or the section itself when there is only one.
    pub(super) root: gtk::Widget,
    sections: Vec<gtk::Expander>,
    /// Divider `i` has section `i` above it.
    dividers: Vec<Divider>,
}

impl Sections {
    /// Stack `sections`, top first, built once and never reparented. Hiding a section, or
    /// folding it to its header, lays the dividers out again.
    pub(super) fn new(sections: &[&gtk::Expander]) -> Rc<Sections> {
        let (last, above) = sections.split_last().expect("a section");
        let mut root: gtk::Widget = (*last).clone().upcast();
        let mut dividers: Vec<Divider> = above
            .iter()
            .rev()
            .map(|section| {
                let divider = Divider::new(*section, &root);
                root = divider.paned.clone().upcast();
                divider
            })
            .collect();
        dividers.reverse();
        let stack = Rc::new(Sections {
            root,
            sections: sections.iter().map(|&section| section.clone()).collect(),
            dividers,
        });
        // A paned has its height once the pane is on screen, which is where the dividers placed
        // before then go.
        let fit = {
            let weak = Rc::downgrade(&stack);
            move || {
                if let Some(stack) = weak.upgrade() {
                    stack.fit();
                }
            }
        };
        stack.root.connect_map({
            let fit = fit.clone();
            move |_| fit()
        });
        for section in &stack.sections {
            section.connect_expanded_notify({
                let fit = fit.clone();
                move |_| fit()
            });
            section.connect_visible_notify({
                let fit = fit.clone();
                move |_| fit()
            });
        }
        stack
    }

    /// Which sections are shown and unfolded.
    fn open(&self) -> Vec<bool> {
        self.sections
            .iter()
            .map(|section| section.is_visible() && section.is_expanded())
            .collect()
    }

    /// Lay the dividers out for the sections now open, the lowest first.
    pub(super) fn fit(&self) {
        let open = self.open();
        let evens = evens(&open);
        for (i, divider) in self.dividers.iter().enumerate().rev() {
            divider.fit(open[i], open[i + 1..].contains(&true), evens[i]);
        }
    }

    /// Whether `paned` is one of the dividers, putting it back to an even split if so.
    pub(super) fn reset(&self, paned: &gtk::Paned) -> bool {
        let Some(i) = self.dividers.iter().position(|d| &d.paned == paned) else {
            return false;
        };
        self.dividers[i].reset(evens(&self.open())[i]);
        true
    }

    /// Each divider's share where the reader dragged it, `None` for an even split.
    pub(super) fn shares(&self) -> Vec<Option<f64>> {
        self.dividers.iter().map(|d| d.share.get()).collect()
    }

    /// Put back the shares `shares` gave, top first; a divider with none is at an even split. The
    /// caller fits the dividers once the folds are back too.
    pub(super) fn restore(&self, shares: &[Option<f64>]) {
        for (i, divider) in self.dividers.iter().enumerate() {
            divider.share.set(shares.get(i).copied().flatten());
        }
    }

    /// The dividers as `position/height`, top first.
    #[cfg(feature = "bench")]
    pub(super) fn positions(&self) -> Vec<String> {
        self.dividers
            .iter()
            .map(|d| format!("{}/{}", d.paned.position(), d.paned.height()))
            .collect()
    }
}

/// Each divider's even split: section `i`'s share of the open sections from it down.
fn evens(open: &[bool]) -> Vec<f64> {
    (1..open.len())
        .map(|below| 1.0 / (1 + open[below..].iter().filter(|&&o| o).count()) as f64)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_open_section_gets_an_even_share() {
        assert_eq!(evens(&[true, true, true]), [1.0 / 3.0, 0.5]);
        // Info's old values with Tags hidden or shut.
        assert_eq!(evens(&[true, false, true]), [0.5, 0.5]);
        assert_eq!(evens(&[true; 5])[0], 0.2);
    }
}
