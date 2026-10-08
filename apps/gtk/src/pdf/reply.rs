//! What the render thread answers a PDF tab, other than the textures the views keep: a page's
//! links and glyphs, the bookmarks, the search's matches, the history, a save, and the pages again
//! after a page edit or a read.

use std::rc::Rc;

use accent_core::pdf;
use adw::prelude::*;

use super::Anchor;
use super::protocol::{Reply, Request};
use super::selection::pages_of;
use super::tab::PdfTab;

impl PdfTab {
    /// Forget what is kept of each page that cannot follow it to another number or document: the
    /// strokes the tools know, the links, and the glyphs with the selection made of them — an
    /// export or a rebuild moves the text, and a stale index would paint the selection elsewhere.
    /// Each is asked for again as it is needed. The comments stay until they are read again, so
    /// the Outline pane's list does not empty and fill again under the reader.
    pub(super) fn forget_pages(&self) {
        self.view.clear_inks();
        self.links.borrow_mut().clear();
        self.glyphs.borrow_mut().clear();
        self.clear_selection();
    }

    /// Everything from the render thread that is not a texture.
    pub(super) fn on_reply(self: &Rc<Self>, reply: Reply) {
        match reply {
            Reply::Links(page, links) => {
                self.links.borrow_mut().insert(page, links);
            }
            Reply::Comments(page, comments) => {
                let was = self.comments.borrow_mut().insert(page, comments.clone());
                // The Outline pane lists them: refilled only when a page's comments change, not
                // for each of a long document's pages read.
                // ponytail: each such page refills the whole outline, every bookmark and comment
                // row compared again; comments on thousands of pages pay that once a page as the
                // walk reads them. One refill a frame is the upgrade if that ever shows.
                if was.unwrap_or_default() != comments {
                    self.on_outline.emit(self);
                }
            }
            Reply::Text(page, glyphs) => self.text_landed(page, glyphs),
            Reply::Outline(outline, info) => {
                *self.outline.borrow_mut() = outline;
                *self.info.borrow_mut() = info;
                self.on_outline.emit(self);
            }
            Reply::Found { query, page, hits } => self.found(query, page, hits),
            Reply::Highlights(map) => self.view.set_highlights(map),
            Reply::Exported(result) => {
                if let Some(f) = self.on_export.get() {
                    f(self, result);
                }
            }
            Reply::PageChanged(page, area) => {
                // The reading view keeps painting what it has until the new render arrives; the
                // strip has only a stand-in, which `refresh_page` drops, so it asks for another.
                self.view.refresh_page(page, area);
                self.thumbs.queue_draw();
                // An export writes each highlight's quote in, which is a comment from now on, on
                // this page whether it is on screen or not: the Outline pane lists them all.
                self.links.borrow_mut().remove(&page);
                self.ask(Request::Links(page));
                if self.wants_inks() {
                    self.ask_inks();
                }
                self.save_soon();
            }
            Reply::Inks { page, inks, erases } => self.view.set_inks(page, inks, erases),
            Reply::History { undo, redo } => {
                self.history.set((undo, redo));
                self.on_history.emit(self);
            }
            Reply::Saved(etag) => {
                self.saved.set(Some(etag));
                self.on_saved.emit(self);
            }
            Reply::SaveFailed(why) => {
                if let Some(f) = self.on_save_failed.get() {
                    f(self, why);
                }
            }
            Reply::Repaged { sizes, edit, step } => self.repaged(sizes, edit, step),
            Reply::Imported { name, pages } => {
                if let Some(f) = self.on_imported.get() {
                    f(self, &name, pages);
                }
            }
            Reply::Reloaded(sizes) => self.reloaded(sizes),
            Reply::Failed(message) => {
                #[cfg(feature = "bench")]
                self.opens.set(self.opens.get() + 1);
                self.fail(&message);
            }
            // What the watcher says too, for a file it watches; this is the render thread
            // finding out first, or for a file nothing watches.
            Reply::Changed => self.refresh(),
            // Textures never reach here; `PdfView::deliver` keeps those.
            Reply::Tile(..) | Reply::Lowres { .. } => {}
        }
    }

    /// A page's glyphs landed: what a drag waiting on them and a link followed into the page need.
    fn text_landed(self: &Rc<Self>, page: usize, glyphs: Vec<pdf::Glyph>) {
        self.glyphs.borrow_mut().insert(page, glyphs);
        // The drag that asked for them is usually still going, so answer it now rather
        // than making the user drag again. A drag across a page break waits for the last
        // page it covers: `select` needs all of them to know where the middle ones end.
        let waiting = self
            .pending_select
            .get()
            .filter(|span| pages_of(*span).contains(&page));
        if let Some(span) = waiting {
            let have = self.glyphs.borrow();
            if pages_of(span).all(|at| have.contains_key(&at)) {
                drop(have);
                self.pending_select.set(None);
                self.select(span);
            }
        }
        // Or a link is waiting for this page, which is Follow Link into the document.
        if self.pending_show.get().is_some_and(|(at, _)| at == page) {
            self.apply_show();
        }
    }

    /// One page's matches for search `query`.
    fn found(self: &Rc<Self>, query: u64, page: usize, hits: Vec<Vec<pdf::Rect>>) {
        // A result for a query the user has already moved past.
        if query != self.query.get() {
            return;
        }
        let found: Vec<pdf::Rect> = hits
            .iter()
            .filter_map(|hit| hit.iter().copied().reduce(pdf::Rect::union))
            .collect();
        if found.is_empty() {
            return;
        }
        // Page order is the order a reader steps through them, and the thread walks from
        // the page being read to the end and round from the first — so this page's
        // matches go among the ones already found where a binary search places them,
        // without sorting the list again per reply.
        let mut matches = self.matches.borrow_mut();
        let at = matches.partition_point(|(seen, _)| *seen <= page);
        matches.splice(at..at, found.iter().map(|rect| (page, *rect)));
        drop(matches);
        let count = found.len();
        self.view.add_marks(page, found);
        match self.current.get() {
            // The first page with a hit, the walk having started on the page being read.
            None if self.jump.take() => self.show_match(at),
            // A page before the current match, the walk having wrapped round.
            Some(current) if at <= current => self.current.set(Some(current + count)),
            _ => {}
        }
        self.on_matches.emit(self);
    }

    /// The pages were put in, taken out or moved: everything kept of a page follows it.
    fn repaged(self: &Rc<Self>, sizes: Vec<(f32, f32)>, edit: pdf::PageEdit, step: u32) {
        let anchor = self.view.anchor();
        let map = |page| edit.map(page);
        // One cache for both views, so it moves once; each view moves what it is still
        // waiting on for a page.
        self.view.cache().borrow_mut().repage(map);
        self.view.repage(map);
        self.thumbs.repage(map);
        self.forget_pages();
        let moved = std::mem::take(&mut *self.comments.borrow_mut());
        *self.comments.borrow_mut() = moved
            .into_iter()
            .filter_map(|(page, comments)| Some((map(page)?, comments)))
            .collect();
        self.view.set_sizes(sizes.clone());
        self.thumbs.set_sizes(sizes);
        match edit {
            // Land on the new page: putting one in is asking for somewhere to draw, and
            // a jump, so the reader can come back with Back.
            pdf::PageEdit::Insert { at, .. } => self.goto_page(at),
            // The page the reader was on, wherever it went — or, deleted, the one that
            // took its place.
            _ => {
                let page = edit.map(anchor.page).unwrap_or(anchor.page);
                let anchor = Anchor { page, ..anchor }.clamped(self.page_count());
                self.view.scroll_to(anchor);
            }
        }
        // The highlights go with their pages at once. The notes holding them are
        // rewritten behind this (`connect_repaged`), and the index's answer after that
        // replaces these.
        for link in self.notes.borrow_mut().iter_mut() {
            link.page = edit.map(link.page).unwrap_or(link.page);
        }
        // Asked again under the new numbers: the bookmarks' pages, every page's comments, where
        // the notes' highlights land, the strokes a tool in hand needs, the links and comments on
        // screen, and the search's matches.
        self.ask(Request::Outline);
        self.ask(Request::Comments(0));
        self.ask(Request::Highlights(self.notes.borrow().clone()));
        if self.wants_inks() {
            self.ask_inks();
        }
        self.ask_links();
        let (searched, options) = self.searched.borrow().clone();
        if !searched.is_empty() {
            self.find(&searched, options, false);
        }
        // The page count changed, or the page the reader is on did, with no scroll.
        self.on_page.emit(self);
        // The strip's buttons are over whichever page is under the pointer now.
        self.hover_thumbnail();
        if let Some(f) = self.on_repaged.get() {
            f(self, edit, step);
        }
        self.save_soon();
    }

    /// The file was read, on opening or again: its pages are these.
    fn reloaded(self: &Rc<Self>, sizes: Vec<(f32, f32)>) {
        #[cfg(feature = "bench")]
        self.opens.set(self.opens.get() + 1);
        // Whatever the far end had that we did not is in hand now, so a refusal after
        // this is a new conflict and worth saying again.
        self.clear_conflict();
        if self.failed.replace(false) {
            self.stack.set_visible_child_name("view");
        }
        // The anchor is taken now rather than when the reload was asked for: the reader
        // may have moved while the file was being re-read. Or where they were when the
        // file stopped opening, the pages having gone since.
        let anchor = self.resume.take().unwrap_or_else(|| self.view.anchor());
        let anchor = anchor.clamped(sizes.len());
        self.view.forget_textures();
        self.thumbs.forget_textures();
        self.forget_pages();
        self.comments
            .borrow_mut()
            .retain(|page, _| *page < sizes.len());
        self.view.set_sizes(sizes.clone());
        self.thumbs.set_sizes(sizes);
        match self.pending.take() {
            // The document just opened: go where the session left the reader, the pages
            // fading in, or crossfading from the spinner that was up in their place. A
            // reload, which a LaTeX build makes every few seconds, just shows them.
            Some(place) => {
                self.view.goto_page(place.page, None);
                match self.stack.visible_child_name().as_deref() {
                    Some("opening") => self.stack.set_visible_child_name("view"),
                    _ => crate::widgets::fade_in(&self.view),
                }
            }
            None => self.view.scroll_to(anchor),
        }
        self.ask(Request::Outline);
        // ponytail: every open and every reload reads every page's annotations for the Outline
        // pane, shown or not: 552 pages in 0.3 s, off the main loop and between tiles. Asking
        // only while the pane shows the document is the upgrade if a LaTeX build's reloads ever
        // make it count.
        self.ask(Request::Comments(0));
        self.ask_links();
        // The strokes went with the old document, and a tool in hand needs this one's.
        if self.wants_inks() {
            self.ask_inks();
        }
        // A link followed into a document that was still opening waits here.
        if let Some((page, sel)) = self.pending_show.get() {
            self.show_link(page, sel);
        }
        self.on_open.emit(self);
    }
}
