//! draw.io's live preview (`mxGraphHandler.livePreview`, `mxVertexHandler.livePreview`): while a
//! move, a resize or a turn is under way, the page is painted as the drag would leave it — the
//! cells where they are going, at full strength, and the edges on them routed again. On a frame
//! the pointer moved in, a copy of the page takes the edit its release will make and is built
//! into a display list of its own; a move of more than [`MAX_LIVE`] cells moves a plain box
//! instead, as draw.io's does.

use accent_drawio::edit::{self, Moving};
use accent_drawio::{Page, Rect, Scene};
use gtk::subclass::prelude::*;

use super::{DiagramView, Edit};
use crate::diagram::geometry::Sheet;
use crate::diagram::paint::Cache;
use crate::diagram::props;

/// The most cells a move shows live, those inside the moved ones counted
/// (`mxGraphHandler.maxLivePreview`, Graph.js 26825).
const MAX_LIVE: usize = 32;

/// What a drag shows from its first frame past the slop to its release.
pub(super) enum Preview {
    /// The page as the drag would leave it.
    Live(Box<Live>),
    /// A move of more than [`MAX_LIVE`] cells: its box moves, the cells stay.
    Boxed,
}

pub(super) struct Live {
    /// The page the drag started on, the edges a move takes already let go of the shapes they
    /// leave: the part of a move that does not depend on how far, done once.
    base: Page,
    moving: Option<Moving>,
    /// The edit [`Live::scene`] shows; `None` until the first frame.
    pub shown: Option<Edit>,
    pub scene: Scene,
    /// Per prim, in page units: what culling tests against.
    pub bounds: Vec<Rect>,
    /// The scene's labels and pictures as laid out for it, its prims numbered apart from the
    /// sheet's.
    pub cache: Cache,
}

impl Preview {
    fn start(sheet: &Sheet, edit: &Edit) -> Preview {
        let mut base = sheet.page.clone();
        let moving = match edit {
            Edit::Move { ids, .. } => match edit::start_move(&mut base, ids) {
                Ok(moving) if moving.count <= MAX_LIVE => Some(moving),
                _ => return Preview::Boxed,
            },
            _ => None,
        };
        Preview::Live(Box::new(Live {
            base,
            moving,
            shown: None,
            scene: Scene::default(),
            bounds: Vec::new(),
            cache: Cache::default(),
        }))
    }
}

impl Live {
    /// Show `edit`, unless the scene already does: the grid holds a move still over many frames.
    /// What was laid out for the frame before (the sheet's, on the first) is kept for every prim
    /// that paints the same thing.
    fn show(&mut self, edit: &Edit, sheet: &Sheet, sheet_cache: &Cache) {
        if self.shown.as_ref() == Some(edit) {
            return;
        }
        let mut page = self.base.clone();
        if let Err(e) = apply(&mut page, edit, self.moving.as_ref()) {
            tracing::debug!("diagram preview refused: {e}");
        }
        let scene = accent_drawio::scene_with(&page, &sheet.ctx);
        let cache = match self.shown {
            None => sheet_cache.carried(&sheet.scene.prims, &scene.prims),
            Some(_) => self.cache.carried(&self.scene.prims, &scene.prims),
        };
        self.bounds = scene.prims.iter().map(|p| p.bounds()).collect();
        self.scene = scene;
        self.cache = cache;
        self.shown = Some(edit.clone());
    }
}

/// What the tab does to the page on the drag's release (`DiagramTab::apply`), on a copy: a move
/// as the second half `start_move` left for it.
fn apply(
    page: &mut Page,
    edit: &Edit,
    moving: Option<&Moving>,
) -> Result<(), accent_drawio::Error> {
    match edit {
        Edit::Move { delta, .. } => {
            if let Some(moving) = moving {
                moving.shift(page, delta.x, delta.y);
            }
            Ok(())
        }
        Edit::Resize { id, rect } => edit::resize(page, id, *rect),
        Edit::Rotate { id, degrees } => {
            let value = props::rotation(*degrees);
            let ids = std::slice::from_ref(id);
            edit::set_styles(page, ids, &[("rotation", value.as_deref())])
        }
        _ => Ok(()),
    }
}

impl DiagramView {
    /// Bring the preview up to the pointer, once a frame: started on the first frame a move, a
    /// resize or a turn is past the slop, gone once none is.
    pub(super) fn update_preview(&self, sheet: &Sheet) {
        let imp = self.imp();
        let edit = match imp.drag.borrow().as_ref() {
            Some(drag) if imp.moved.get() => {
                self.drag_edit(drag, imp.pointer.get(), imp.free.get())
            }
            _ => None,
        };
        let mut preview = imp.preview.borrow_mut();
        let Some(edit) = edit else {
            *preview = None;
            return;
        };
        let preview = preview.get_or_insert_with(|| Preview::start(sheet, &edit));
        if let Preview::Live(live) = preview {
            live.show(&edit, sheet, &imp.cache);
        }
    }
}

#[cfg(feature = "bench")]
impl DiagramView {
    /// Paint one frame of a move of `ids` by `delta` (with none, of no drag at all), as the
    /// frame clock would: the time the preview took to catch up, and the whole snapshot, in ms.
    pub fn bench_move(
        &self,
        ids: &[accent_drawio::CellId],
        delta: Option<accent_drawio::Point>,
    ) -> (f64, f64) {
        use gtk::prelude::*;
        let imp = self.imp();
        let Some(sheet) = self.sheet() else {
            return (0.0, 0.0);
        };
        match delta {
            Some(delta) => {
                *imp.drag.borrow_mut() = Some(super::drag::Drag::Move {
                    from: accent_drawio::Point::default(),
                    ids: ids.to_vec(),
                    click: None,
                    bounds: accent_drawio::Rect::default(),
                    guides: Vec::new(),
                });
                imp.moved.set(true);
                imp.free.set(true);
                imp.pointer.set(delta);
            }
            None => {
                imp.drag.take();
            }
        }
        let ms = |t: std::time::Instant| t.elapsed().as_secs_f64() * 1e3;
        let started = std::time::Instant::now();
        self.update_preview(&sheet);
        let update = ms(started);
        let started = std::time::Instant::now();
        let snapshot = gtk::Snapshot::new();
        // The widget's own paint, which GTK would otherwise answer from the frame before.
        WidgetImpl::snapshot(imp, &snapshot);
        let _ = snapshot.to_node();
        (update, ms(started))
    }
}
