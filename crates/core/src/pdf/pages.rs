//! PDF page insertion, deletion, movement and snapshots for restoring deleted pages.

use std::io::Write;
use std::os::raw::{c_int, c_ulong};

use crate::{Error, Result};
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
            return Err(Error::Invalid(format!(
                "cannot insert page {at} into {count}"
            )));
        }
        let beside = at.saturating_sub(1);
        let rect = self
            .doc()
            .pages()
            .page_size(beside as PdfPageIndex)
            .map_err(|e| Error::Pdf(format!("page {beside}: {e:?}")))?;
        let size = paper((rect.width().value, rect.height().value));
        let mut page = self
            .doc_mut()
            .pages_mut()
            .create_page_at_index(size, at as PdfPageIndex)
            .map_err(|e| Error::Pdf(format!("insert page {at}: {e:?}")))?;
        // Manual, as every other mutation in this module sets it: dropping the page otherwise
        // re-serialises a content stream we never wrote.
        page.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
        Ok(())
    }

    /// Take a page out. Refused for the last one: a PDF without pages is not one a reader opens.
    pub fn delete_page(&mut self, page: usize) -> Result<()> {
        let _guard = lock();
        if self.doc().pages().len() < 2 {
            return Err(Error::Invalid("a PDF keeps at least one page".to_string()));
        }
        self.page(page)?
            .delete()
            .map_err(|e| Error::Pdf(format!("delete page {page}: {e:?}")))
    }

    /// Snapshot the document for page-delete Undo. Restoring the whole document preserves links
    /// and bookmarks that a page copy would lose.
    ///
    /// Uses an unnamed cache file so large PDFs consume disk rather than memory per history
    /// entry. The file disappears when the last document handle closes, including after a crash.
    pub(super) fn snapshot(&self) -> Result<PdfDoc> {
        let bytes = self.save()?;
        let dir = crate::config::xdg("XDG_CACHE_HOME", ".cache").join("accent");
        let file = std::fs::create_dir_all(&dir).and_then(|()| {
            let mut file = tempfile::tempfile_in(&dir)?;
            file.write_all(&bytes)?;
            Ok(file)
        });
        PdfDoc::from_file(file.map_err(|e| Error::io(dir.display(), e))?)
    }

    /// Insert all pages of `source` at `at` in order, returning their count. `at` may equal the
    /// page count to append. Text, annotations and resources are copied; links to other pages
    /// and source bookmarks are not.
    ///
    /// Clear copied annotations' `/Parent` references while `source` is open. Otherwise saving
    /// can follow source objects into freed memory or unrelated destination objects. Popups
    /// still open with their markup; form fields are not imported.
    pub fn import_pages(&mut self, source: &PdfDoc, at: usize) -> Result<usize> {
        let _guard = lock();
        let count = self.doc().pages().len() as usize;
        if at > count {
            return Err(Error::Invalid(format!(
                "cannot import pages at {at} of {count}"
            )));
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
            .map_err(|e| Error::Pdf(format!("import {added} pages at {at}: {e:?}")))?;
        for page in at..at + added {
            let mut p = self.page(page)?;
            p.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
            cut_parents(&p);
        }
        Ok(added)
    }

    /// Move the page itself from `from` to `to`, preserving links and bookmarks. Raw
    /// `FPDF_MovePages` is required because pdfium-render offers only copy and delete, which
    /// invalidate those references.
    pub fn move_page(&mut self, from: usize, to: usize) -> Result<()> {
        let _guard = lock();
        let doc = self.doc();
        let count = doc.pages().len() as usize;
        if from >= count || to >= count {
            return Err(Error::Invalid(format!(
                "cannot move page {from} to {to} of {count}"
            )));
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
            false => Err(Error::Pdf(format!(
                "pdfium would not move page {from} to {to}"
            ))),
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
