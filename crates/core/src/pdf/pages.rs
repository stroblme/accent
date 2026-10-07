//! Which pages a document has and in what order: a blank page put in, another document's pages
//! put in, a page taken out, a page moved somewhere else, and the whole document kept for a page
//! taken out to come back.

use std::io::Write;
use std::os::raw::{c_int, c_ulong};

use anyhow::{Context, Result, anyhow};
use pdfium_render::prelude::*;

use super::doc::paper;
use super::{PdfDoc, lock};

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

    /// The document as it now stands, as a second document of its own: what a page delete keeps
    /// for its Undo to swap back in (`ledger`), since pdfium cannot hand back a page once it is
    /// gone, and a copy of one would lose the links into it, out of it and the bookmarks naming it.
    ///
    /// It is read from a file in the cache dir rather than held in memory, so a scanned PDF of a
    /// hundred megabytes costs that much disk per delete in the history, not memory. The file has
    /// no name: it goes with the last document reading it, a crash included.
    pub(super) fn snapshot(&self) -> Result<PdfDoc> {
        let bytes = self.save()?;
        let dir = crate::config::xdg("XDG_CACHE_HOME", ".cache").join("accent");
        let file = std::fs::create_dir_all(&dir).and_then(|()| {
            let mut file = tempfile::tempfile_in(&dir)?;
            file.write_all(&bytes)?;
            Ok(file)
        });
        PdfDoc::from_file(file.with_context(|| format!("snapshot into {}", dir.display()))?)
    }

    /// Every page of `source` in at `at`, in order, each with its text, its annotations and its
    /// resources. pdfium's copy of a page drops its links to other pages, as for any copy, and
    /// `source`'s bookmarks stay behind. `at` may be the page count, which appends. How many
    /// pages went in.
    ///
    /// The copy also keeps every annotation's `/Parent` as it was — a pop-up's markup, a widget's
    /// form field — naming an object of `source`, which a save follows into freed memory once
    /// `source` has closed, and writes as whichever object of this document has its number while
    /// it is open. Each is cut loose here, while `source` is still open: a pop-up opens with its
    /// markup, and no form field comes along.
    pub fn import_pages(&mut self, source: &PdfDoc, at: usize) -> Result<usize> {
        let _guard = lock();
        let count = self.doc().pages().len() as usize;
        if at > count {
            return Err(anyhow!("cannot import pages at {at} of {count}"));
        }
        let pages = source.doc().pages();
        let added = pages.len() as usize;
        self.doc_mut()
            .pages_mut()
            .copy_page_range_from_document(
                source.doc(),
                pages.as_range_inclusive(),
                at as PdfPageIndex,
            )
            .map_err(|e| anyhow!("import {added} pages at {at}: {e:?}"))?;
        for page in at..at + added {
            let mut p = self.page(page)?;
            p.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
            cut_parents(&p);
        }
        Ok(added)
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

/// Replace every annotation's `/Parent` on a loaded page with an empty string, which names
/// nothing: pdfium has no call that takes a key out. The caller holds the lock.
fn cut_parents(p: &PdfPage<'_>) {
    let bindings = p.bindings();
    // SAFETY: the page handle is `p`'s own and alive while it is borrowed; the lock is held, so no
    // other thread is inside pdfium; each annotation handle is closed before the next is taken.
    unsafe {
        let page = bindings.get_handle_from_page(p);
        for index in 0..bindings.FPDFPage_GetAnnotCount(page) {
            let annot = bindings.FPDFPage_GetAnnot(page, index);
            if annot.is_null() {
                continue;
            }
            if bindings.is_true(bindings.FPDFAnnot_HasKey(annot, "Parent")) {
                bindings.FPDFAnnot_SetStringValue_str(annot, "Parent", "");
            }
            bindings.FPDFPage_CloseAnnot(annot);
        }
    }
}
