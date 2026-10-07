//! Another PDF dropped onto a document's pages: its pages go in between the two under the
//! pointer, a line in that gap showing where while the drag is over them.
//!
//! A file from another application is taken here. A row of the Files tree is the pane's drop
//! (`App::pdf_drop`): its sheet lies over the pages while a drag from the tree is in flight.

use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, glib, graphene};

use super::geometry::gap_at;
use super::protocol::Request;
use super::tab::PdfTab;
use crate::doc::{self, Kind};

impl PdfTab {
    /// Put every page of the PDF at `source`, a file on this machine called `name`, in at page
    /// `at`, after the last page for `None`: one step Undo takes back, saved as a page edit is. A
    /// document still opening is handed it once its render thread starts.
    pub fn import_pages(&self, at: Option<usize>, source: PathBuf, name: String) {
        let request = Request::Import { at, source, name };
        match self.tx.borrow().as_ref() {
            Some(tx) => {
                let _ = tx.send(request);
            }
            None => self.waiting.borrow_mut().push(request),
        }
    }

    /// The gap a drop at `(x, y)` of `over` puts another PDF's pages into: gap `g` is before page
    /// `g`, the one nearer the pointer of the two either side of the page under it. `None` while
    /// the pages are not known.
    pub fn drop_gap(&self, over: &gtk::Widget, x: f64, y: f64) -> Option<usize> {
        let at = graphene::Point::new(x as f32, y as f32);
        let at = over.compute_point(&self.view, &at)?;
        let (layout, top) = self.view.placement();
        (!layout.pages.is_empty()).then(|| gap_at(&layout, f64::from(at.y()) + top))
    }

    /// Show the line across `gap`, or take it away.
    pub fn show_drop(&self, gap: Option<usize>) {
        self.view.set_drop_gap(gap);
    }

    /// Take one PDF dropped from another application onto the pages.
    pub(super) fn wire_drop(self: &Rc<Self>) {
        let target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        // Read as the drag comes in, so the motion can tell one PDF from anything else.
        target.set_preload(true);
        let aim = glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[upgrade_or]
            gdk::DragAction::empty(),
            move |target: &gtk::DropTarget, x: f64, y: f64| {
                let carried = target.value().and_then(|value| tab.droppable(&value));
                let gap = carried.and_then(|_| tab.drop_gap(tab.view.upcast_ref(), x, y));
                tab.show_drop(gap);
                match gap {
                    Some(_) => gdk::DragAction::COPY,
                    None => gdk::DragAction::empty(),
                }
            }
        );
        target.connect_enter(aim.clone());
        target.connect_motion(aim);
        target.connect_leave(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |_| tab.show_drop(None)
        ));
        target.connect_drop(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[upgrade_or]
            false,
            move |_, value, x, y| {
                tab.show_drop(None);
                let source = tab.droppable(value);
                let gap = tab.drop_gap(tab.view.upcast_ref(), x, y);
                let (Some(source), Some(gap)) = (source, gap) else {
                    return false;
                };
                let name = doc::file_name(&source.to_string_lossy()).to_string();
                tab.import_pages(Some(gap), source, name);
                true
            }
        ));
        self.view.add_controller(target);
    }

    /// Where a drop into `gap` lands in `over`: the middle of the gap, halfway across the view.
    /// What `ACCENT_BENCH_PDF=insert:` aims XTEST at.
    #[cfg(feature = "bench")]
    pub fn drop_point(&self, gap: usize, over: &gtk::Widget) -> Option<(f32, f32)> {
        let (layout, top) = self.view.placement();
        let y = super::geometry::gap_middle(&layout, gap) - top as f32;
        let x = self.view.width() as f32 / 2.0;
        let at = self.view.compute_point(over, &graphene::Point::new(x, y))?;
        Some((at.x(), at.y()))
    }

    /// The gap whose line shows, while a PDF is dragged over the pages.
    #[cfg(feature = "bench")]
    pub fn drop_shown(&self) -> Option<usize> {
        self.view.drop_gap()
    }

    /// The one PDF a drop from another application carries, unless it is this document's file.
    fn droppable(&self, value: &glib::Value) -> Option<PathBuf> {
        let [file] = crate::tree::dropped_paths(value)?.try_into().ok()?;
        let pdf = doc::kind_of(&file.to_string_lossy()) == Kind::Pdf;
        let own = file.canonicalize().ok() == self.path().canonicalize().ok();
        (pdf && !own).then_some(file)
    }
}
