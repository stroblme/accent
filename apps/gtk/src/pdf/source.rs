//! The page's half of SyncTeX (`crate::synctex`): the point Go to Source starts from, and the
//! line of text Show in PDF brings into view and marks a moment.

use std::rc::Rc;
use std::time::Duration;

use accent_core::pdf;
use adw::prelude::*;
use gtk::glib;

use super::tab::PdfTab;

/// How long Show in PDF's mark stays on the line of text it went to.
const MARK: Duration = Duration::from_millis(1500);

impl PdfTab {
    /// Where Go to Source looks: the page and point the page's menu was opened on or Ctrl was
    /// clicked at, else the middle of the view, which is what the palette's Go to Source means.
    pub fn source_point(&self) -> Option<(usize, f32, f32)> {
        self.pointed.take().or_else(|| {
            let (w, h) = (self.view.width(), self.view.height());
            self.view.page_point(f64::from(w) / 2.0, f64::from(h) / 2.0)
        })
    }

    /// Bring `rect` of `page` into view and mark it as a selection is drawn, until [`MARK`] has
    /// passed or a selection replaces it. A jump, so Back returns to where the reader was; a
    /// document still opening does it once its pages are known ([`PdfTab::show_pending_spot`]).
    pub fn show_spot(self: &Rc<Self>, page: usize, rect: pdf::Rect) {
        if self.opening() {
            return self.spot.set(Some((page, rect)));
        }
        self.jumping();
        self.clear_selection();
        self.view.set_selection(vec![(page, vec![rect])]);
        self.view.reveal(page, rect);
        glib::timeout_add_local_once(
            MARK,
            glib::clone!(
                #[weak(rename_to = tab)]
                self,
                move || {
                    if tab.selected.borrow().is_empty() {
                        tab.view.set_selection(Vec::new());
                    }
                }
            ),
        );
    }

    /// Point Go to Source at `(x, y)` on `page`, as a secondary click there does. Only drills ask.
    #[cfg(feature = "bench")]
    pub fn point_at(&self, page: usize, x: f32, y: f32) {
        self.pointed.set(Some((page, x, y)));
    }

    /// The line of text Show in PDF marks, while it is marked. Only drills ask.
    #[cfg(feature = "bench")]
    pub fn marked(&self) -> Option<(usize, pdf::Rect)> {
        let marked = self.view.selection();
        let (page, rects) = marked.first()?;
        Some((*page, *rects.first()?))
    }

    /// What Show in PDF asked for while the document was opening, now its pages are there.
    pub fn show_pending_spot(self: &Rc<Self>) {
        if self.page_count() > 0
            && let Some((page, rect)) = self.spot.take()
        {
            self.show_spot(page, rect);
        }
    }
}
