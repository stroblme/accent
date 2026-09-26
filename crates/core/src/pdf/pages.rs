//! Which pages a document has and in what order: a blank page put in, a page taken out, a page
//! moved somewhere else, and a page taken out put back.

use std::os::raw::{c_int, c_ulong};

use anyhow::{Context, Result, anyhow};
use pdfium_render::prelude::*;

use super::doc::paper;
use super::{PdfDoc, lock, pdfium};

impl PdfDoc {
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

    /// Take a page out and hand it back as a PDF of its own — its content, the resources it uses
    /// and its annotations, ink included — for [`PdfDoc::put_page`] to put back: pdfium cannot
    /// hand back a page once it is gone.
    ///
    /// What points at the page, or from it to another, does not come back with it. pdfium's copy
    /// drops every reference to a page outside it, so a link on the page to another loses its
    /// target; and the bookmarks, links and named destinations elsewhere keep naming the page
    /// deleted here, not the copy put back.
    pub fn take_page(&mut self, page: usize) -> Result<Vec<u8>> {
        let copy = {
            let pdfium = pdfium()?;
            let _guard = lock();
            let mut copy = pdfium.create_new_pdf().context("create pdf")?;
            copy.pages_mut()
                .copy_page_from_document(self.doc(), page as PdfPageIndex, 0)
                .map_err(|e| anyhow!("copy page {page}: {e:?}"))?;
            copy.save_to_bytes().context("save the page")?
        };
        self.delete_page(page)?;
        Ok(copy)
    }

    /// Put a page [`PdfDoc::take_page`] took out back in at `at`.
    pub fn put_page(&mut self, at: usize, page: &[u8]) -> Result<()> {
        let pdfium = pdfium()?;
        let _guard = lock();
        let count = self.doc().pages().len() as usize;
        if at > count {
            return Err(anyhow!("cannot put page {at} back into {count}"));
        }
        let source = pdfium
            .load_pdf_from_byte_slice(page, None)
            .context("open the page kept")?;
        self.doc_mut()
            .pages_mut()
            .copy_page_from_document(&source, 0, at as PdfPageIndex)
            .map_err(|e| anyhow!("put page {at} back: {e:?}"))
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
