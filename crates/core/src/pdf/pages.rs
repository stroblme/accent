//! Which pages a document has and in what order: a blank page put in, a page taken out, a page
//! moved somewhere else.

use std::os::raw::{c_int, c_ulong};

use anyhow::{Result, anyhow};
use pdfium_render::prelude::*;

use super::doc::paper;
use super::{PdfDoc, lock};

/// One change to a document's pages.
///
/// Everything a viewer keeps of a page — its renders, its glyphs, its ink history — is filed under
/// the page's number, and [`PageEdit::map`] is how each of them follows the edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageEdit {
    /// A blank page goes in at this index, the size of the page before it.
    Insert(usize),
    /// The page at this index goes.
    Delete(usize),
    /// The page at `from` is taken out and put back so that it ends up at `to`.
    Move { from: usize, to: usize },
}

impl PageEdit {
    /// Where the page that was at `page` is after the edit; `None` for the one deleted.
    pub fn map(self, page: usize) -> Option<usize> {
        match self {
            PageEdit::Insert(at) => Some(page + usize::from(page >= at)),
            PageEdit::Delete(at) if page == at => None,
            PageEdit::Delete(at) => Some(page - usize::from(page > at)),
            PageEdit::Move { from, to } if page == from => Some(to),
            // Taking `from` out moves every page after it up one; putting it back at `to` moves
            // every page from there on down one.
            PageEdit::Move { from, to } => {
                let out = page - usize::from(page > from);
                Some(out + usize::from(out >= to))
            }
        }
    }
}

impl PdfDoc {
    /// Make one change to the pages.
    ///
    /// Nothing reaches the disk here: [`PdfDoc::save`] is the second half, as it is for ink.
    pub fn edit_pages(&mut self, edit: PageEdit) -> Result<()> {
        match edit {
            PageEdit::Insert(at) => self.insert_page(at),
            PageEdit::Delete(page) => self.delete_page(page),
            PageEdit::Move { from, to } => self.move_page(from, to),
        }
    }

    /// A blank page at `at`, the size of the page before it (the first page's, at the front):
    /// what a paper notebook does when the page runs out, and what keeps a drawing readable in any
    /// viewer — one MediaBox per page, none of them growing. `at` may be the page count, which
    /// appends.
    pub fn insert_page(&mut self, at: usize) -> Result<()> {
        let _guard = lock();
        let count = self.doc().pages().len() as usize;
        if count == 0 || at > count {
            return Err(anyhow!("cannot insert page {at} into {count}"));
        }
        let beside = at.saturating_sub(1);
        let rect = self
            .doc()
            .pages()
            .page_size(beside as PdfPageIndex)
            .map_err(|e| anyhow!("page {beside}: {e:?}"))?;
        let size = paper((rect.width().value, rect.height().value));
        let mut page = self
            .doc_mut()
            .pages_mut()
            .create_page_at_index(size, at as PdfPageIndex)
            .map_err(|e| anyhow!("insert page {at}: {e:?}"))?;
        // Manual, as every other mutation in this module sets it: dropping the page otherwise
        // re-serialises a content stream we never wrote.
        page.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
        Ok(())
    }

    /// Take a page out. Refused for the last one: a PDF without pages is not one a reader opens.
    pub fn delete_page(&mut self, page: usize) -> Result<()> {
        let _guard = lock();
        if self.doc().pages().len() < 2 {
            return Err(anyhow!("a PDF keeps at least one page"));
        }
        self.page(page)?
            .delete()
            .map_err(|e| anyhow!("delete page {page}: {e:?}"))
    }

    /// Move the page at `from` so that it ends up at `to`.
    ///
    /// The page itself moves — the page tree is reordered and nothing is copied — so a bookmark or
    /// a link that points at it still points at it wherever it lands. A copy and a delete, which
    /// is all pdfium-render offers, would leave both pointing at a page that is gone. Hence the one
    /// raw call below: `FPDF_MovePages` is bound but not wrapped.
    pub fn move_page(&mut self, from: usize, to: usize) -> Result<()> {
        let _guard = lock();
        let doc = self.doc();
        let count = doc.pages().len() as usize;
        if from >= count || to >= count {
            return Err(anyhow!("cannot move page {from} to {to} of {count}"));
        }
        let bindings = doc.bindings();
        let pages = [from as c_int];
        // SAFETY: the handle is this open document's own, alive for as long as `self` is; `pages`
        // outlives the call and its length is what is passed; the lock is held, so no other thread
        // is inside pdfium; and no page of the document is loaded — every one this module loads is
        // dropped before its call returns, and `&mut self` rules out one borrowed from here — so
        // pdfium-render's table of loaded pages by index, which this reorder does not update, has
        // nothing in it to go stale.
        let moved = unsafe {
            bindings.FPDF_MovePages(
                bindings.get_handle_from_document(doc),
                pages.as_ptr(),
                pages.len() as c_ulong,
                to as c_int,
            )
        };
        match bindings.is_true(moved) {
            true => Ok(()),
            false => Err(anyhow!("pdfium would not move page {from} to {to}")),
        }
    }
}
