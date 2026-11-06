//! Insert pages from a dropped PDF at the gap under the pointer, marked by a drop line, put
//! dropped pictures on the page under it, and copy a page for the strip's Paste Page Here.
//!
//! Handle external files here. Files-tree rows use the pane drop (`App::pdf_drop`,
//! `App::picture_drop`), whose target covers the pages during a tree drag.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::channel;

use accent_core::markdown::is_image;
use accent_core::pdf::{Clip, IDENTITY};
use adw::prelude::*;
use gtk::{gdk, glib, graphene};

use super::clipboard::STEP;
use super::geometry::gap_at;
use super::pictures::Incoming;
use super::protocol::Request;
use super::tab::PdfTab;
use crate::doc::{self, Kind};
use crate::organize::{self, Copied};

/// What a drop from another application carries onto the pages.
enum Dropped {
    /// Another PDF, whose pages go in between two.
    Pages(PathBuf),
    /// Image files, which land as pictures on the page.
    Pictures(Vec<PathBuf>),
    /// An image itself, as a browser may drag one.
    Texture(gdk::Texture),
}

impl PdfTab {
    /// Put every page of the PDF at `source`, a file on this machine called `name`, or page
    /// `page` of it alone, in at page `at`, after the last page for `None`: one step Undo takes
    /// back, saved as a page edit is. A document still opening is handed it once its render
    /// thread starts.
    pub fn import_pages(
        &self,
        at: Option<usize>,
        source: PathBuf,
        page: Option<usize>,
        name: String,
    ) {
        let request = Request::Import {
            at,
            source,
            page,
            name,
        };
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

    /// Take one PDF, or pictures, dropped from another application onto the pages.
    pub(super) fn wire_drop(self: &Rc<Self>) {
        let target = gtk::DropTarget::new(glib::Type::INVALID, gdk::DragAction::COPY);
        // An image itself first, as a browser may drag one beside its address.
        target.set_types(&[gdk::Texture::static_type(), gdk::FileList::static_type()]);
        // Read as the drag comes in, so the motion can tell one PDF or pictures from anything
        // else.
        target.set_preload(true);
        let aim = glib::clone!(
            #[weak(rename_to = tab)]
            self,
            #[upgrade_or]
            gdk::DragAction::empty(),
            move |target: &gtk::DropTarget, x: f64, y: f64| {
                let carried = target.value().and_then(|value| tab.droppable(&value));
                let gap = match carried {
                    Some(Dropped::Pages(_)) => tab.drop_gap(tab.view.upcast_ref(), x, y),
                    _ => None,
                };
                tab.show_drop(gap);
                // A picture shows no line: it lands where the pointer is.
                let pictured = matches!(carried, Some(Dropped::Pictures(_) | Dropped::Texture(_)));
                match gap.is_some() || (pictured && tab.view.page_point(x, y).is_some()) {
                    true => gdk::DragAction::COPY,
                    false => gdk::DragAction::empty(),
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
                let under = tab.view.page_point(x, y);
                match (tab.droppable(value), under) {
                    (Some(Dropped::Pages(source)), _) => {
                        let Some(gap) = tab.drop_gap(tab.view.upcast_ref(), x, y) else {
                            return false;
                        };
                        let name = doc::file_name(&source.to_string_lossy()).to_string();
                        tab.import_pages(Some(gap), source, None, name);
                    }
                    (Some(Dropped::Pictures(files)), Some((page, x, y))) => {
                        let read = move || files.iter().map(|f| Incoming::file(f)).collect();
                        tab.drop_pictures(page, (x, y), read);
                    }
                    (Some(Dropped::Texture(texture)), Some((page, x, y))) => {
                        let read = move || vec![Incoming::pasted(&texture)];
                        tab.drop_pictures(page, (x, y), read);
                    }
                    _ => return false,
                }
                true
            }
        ));
        self.view.add_controller(target);
    }

    /// The strip's Copy Page and Paste Page Here: a page copied is pasted as a dropped PDF's pages
    /// go in, from a copy of its document.
    pub(super) fn wire_paste(self: &Rc<Self>) {
        self.organize.connect_copy(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |page| tab.copy_page(page)
        ));
        self.organize.connect_paste(glib::clone!(
            #[weak(rename_to = tab)]
            self,
            move |at, copied| {
                if let Copied::Pdf { file, page, name } = copied {
                    tab.import_pages(Some(at), file.clone(), Some(*page), name.clone());
                }
            }
        ));
    }

    /// Hold `page` for Paste Page Here, in this PDF's strip or another's. The document as it now
    /// stands, strokes not yet saved included, is copied into the cache first, so the page pasted
    /// is the one copied whatever the document goes through since. Nothing is held while the
    /// copy is written over the last one.
    fn copy_page(self: &Rc<Self>, page: usize) {
        organize::hold(None);
        let dir = glib::user_cache_dir().join("accent");
        let file = dir.join("copied-page.pdf");
        let name = doc::file_name(&self.key()).to_string();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return tracing::warn!("copying a page of {name}: {e}");
        }
        let (done, answer) = channel();
        // Without the notes' highlights, which are the vault's and not the page's.
        self.ask(Request::Copy {
            links: Vec::new(),
            color: [0; 3],
            dest: file.clone(),
            done,
        });
        let tab = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            match crate::work::off_thread("page copy", move || answer.recv()).await {
                Some(Ok(Ok(()))) => {
                    organize::hold(Some(Copied::Pdf { file, page, name }));
                    if let Some(tab) = tab.upgrade() {
                        tab.organize.hover();
                    }
                }
                Some(Ok(Err(e))) => tracing::warn!("copying a page of {name}: {e}"),
                _ => {}
            }
        });
    }

    /// The page under `(x, y)` of `over` and the point on it, while the pointer is over a page:
    /// where a picture dropped there lands.
    pub fn page_under(&self, over: &gtk::Widget, x: f64, y: f64) -> Option<(usize, (f32, f32))> {
        let at = graphene::Point::new(x as f32, y as f32);
        let at = over.compute_point(&self.view, &at)?;
        let (page, x, y) = self.view.page_point(f64::from(at.x()), f64::from(at.y()))?;
        Some((page, (x, y)))
    }

    /// Pictures dropped at `at` of `page`, as `read` reads them on a worker, one for each image
    /// and `None` for one that does not read as an image: the first centred there and each after
    /// it a step further down and right, at their natural size up to half the page and wholly on
    /// the paper, as one step Undo takes back. The Select tool holds them, and the window picks it
    /// up ([`PdfTab::connect_pictured`]).
    pub fn drop_pictures(
        self: &Rc<Self>,
        page: usize,
        at: (f32, f32),
        read: impl FnOnce() -> Vec<Option<Incoming>> + Send + 'static,
    ) {
        let tab = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let read = crate::work::off_thread("read the dropped pictures", read).await;
            let (Some(tab), Some(read)) = (tab.upgrade(), read) else {
                return;
            };
            let Some(size) = tab.page_size(page) else {
                return;
            };
            let unread = read.iter().filter(|incoming| incoming.is_none()).count();
            let clips: Vec<Clip> = (read.into_iter().flatten().enumerate())
                .map(|(n, incoming)| {
                    let d = n as f32 * STEP;
                    Clip::Image {
                        place: incoming.place((at.0 + d, at.1 + d), size),
                        picture: incoming.picture,
                    }
                })
                .collect();
            let landed = clips.len();
            tab.paste_clips(page, clips, IDENTITY);
            if let Some(f) = tab.on_pictured.get() {
                f(&tab, landed, unread);
            }
        });
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

    /// What a drop from another application carries: an image, image files, or one PDF unless it
    /// is this document's file.
    fn droppable(&self, value: &glib::Value) -> Option<Dropped> {
        if let Ok(texture) = value.get::<gdk::Texture>() {
            return Some(Dropped::Texture(texture));
        }
        let files = crate::tree::dropped_paths(value)?;
        if files.iter().all(|f| is_image(&f.to_string_lossy())) {
            return Some(Dropped::Pictures(files));
        }
        let [file] = files.try_into().ok()?;
        let pdf = doc::kind_of(&file.to_string_lossy()) == Kind::Pdf;
        let own = file.canonicalize().ok() == self.path().canonicalize().ok();
        (pdf && !own).then_some(Dropped::Pages(file))
    }
}
