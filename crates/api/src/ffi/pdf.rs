//! One open PDF, with everything a reader and a pen need.
//!
//! The desktop keeps this behind a thread per document, because pdfium is serialised by one
//! process-wide lock and a GTK widget must never wait on it. The rule is the same here, but the
//! thread is the caller's: every method takes the session's own lock and then pdfium's, so any
//! thread may call and none may be the main one. A caller that wants its tiles in order runs
//! them on one worker of its own; that is also the only cancellation there is, since uniffi
//! offers none and a call is never interrupted once it has begun. A tile is small enough for
//! that to be the right unit.
//!
//! The undo ledger is `accent_core::pdf::ledger`, the same one the desktop render thread walks.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use accent_core::fs::{self, Etag};
use accent_core::pdf::{self, Ink, NamedInk, PdfDoc};

use crate::ffi::convert::{
    self, Glyph, Highlight, History, InkStyle, Outline, PageArea, PageSize, PdfLinkBox, Point,
    SearchLine, Theme, Tile,
};
use crate::ffi::error::{AccentError, Answer};

struct State {
    doc: PdfDoc,
    /// Where it was opened from, and the stamp the last read or write left. Both `None` for a
    /// document opened from bytes, which has no file behind it to save into.
    path: Option<PathBuf>,
    etag: Option<Etag>,
    ink: Ink,
    /// Per page, so a selection drag does not re-read the text on every move.
    glyphs: HashMap<usize, Vec<pdf::Glyph>>,
    /// Per page, so an eraser that misses costs nothing. Dropped whenever the page's ink changes.
    inks: HashMap<usize, Vec<NamedInk>>,
}

#[derive(uniffi::Object)]
pub struct PdfSession(Mutex<State>);

impl PdfSession {
    fn with<T>(&self, f: impl FnOnce(&mut State) -> Answer<T>) -> Answer<T> {
        f(&mut self.0.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn of(doc: PdfDoc, path: Option<PathBuf>) -> Answer<Self> {
        if doc.page_count() == 0 {
            return Err(AccentError::Failed {
                reason: "this file has no pages".to_string(),
            });
        }
        let etag = path.as_deref().and_then(|p| Etag::of(p).ok());
        Ok(PdfSession(Mutex::new(State {
            doc,
            path,
            etag,
            ink: Ink::default(),
            glyphs: HashMap::new(),
            inks: HashMap::new(),
        })))
    }
}

#[uniffi::export]
impl PdfSession {
    /// Open a document from a file, which is what a vault holds.
    #[uniffi::constructor]
    pub fn open(path: String) -> Answer<Self> {
        if !pdf::available() {
            return Err(AccentError::Failed {
                reason: "libpdfium was not found".to_string(),
            });
        }
        let path = PathBuf::from(path);
        PdfSession::of(PdfDoc::open(&path)?, Some(path))
    }

    /// Open a document from bytes: a `content://` URI, where there is no path to read.
    ///
    /// It cannot [`save`](Self::save); the caller writes what [`save_bytes`](Self::save_bytes)
    /// hands back, through whatever gave it the bytes.
    #[uniffi::constructor]
    pub fn open_bytes(bytes: Vec<u8>) -> Answer<Self> {
        if !pdf::available() {
            return Err(AccentError::Failed {
                reason: "libpdfium was not found".to_string(),
            });
        }
        PdfSession::of(PdfDoc::from_bytes(bytes)?, None)
    }

    pub fn page_count(&self) -> u32 {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .doc
            .page_count() as u32
    }

    /// Every page's size in points, read out of the page tree without loading any of them.
    pub fn page_sizes(&self) -> Answer<Vec<PageSize>> {
        self.with(|s| Ok(convert::all(s.doc.page_sizes()?)))
    }

    // ---------------------------------------------------------------------------- painting

    /// A piece of a page, at `scale` device pixels per point, in device pixels from its top-left.
    // The same shape as `PdfDoc::render_tile`, which is what it forwards to.
    #[allow(clippy::too_many_arguments)]
    pub fn render_tile(
        &self,
        page: u32,
        scale: f32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        theme: Theme,
    ) -> Answer<Tile> {
        self.with(|s| {
            Ok(s.doc
                .render_tile(page as usize, scale, x, y, w, h, theme.into())?
                .into())
        })
    }

    /// A whole page at `scale`: the low-resolution stand-in a viewer paints until its tiles land.
    pub fn render_page(&self, page: u32, scale: f32, theme: Theme) -> Answer<Tile> {
        self.with(|s| {
            Ok(s.doc
                .render_page(page as usize, scale, theme.into())?
                .into())
        })
    }

    // ------------------------------------------------------------------------ text and links

    /// Every character of a page with its box, in the order pdfium reads them. Cached.
    pub fn glyphs(&self, page: u32) -> Answer<Vec<Glyph>> {
        self.with(|s| Ok(convert::all(s.page_glyphs(page as usize)?.to_vec())))
    }

    /// The wikilink a selection of glyphs makes, plus the quads and the quoted text that
    /// re-anchor it when the numbers no longer fit the document.
    pub fn selection_link(
        &self,
        rel: String,
        page: u32,
        start: u32,
        end: u32,
    ) -> Answer<pdf::SelectionLink> {
        self.with(|s| {
            let glyphs = s.page_glyphs(page as usize)?;
            let sel = pdf::Selection {
                page: page as usize,
                start: start as usize,
                end: end as usize,
            };
            Ok(pdf::selection_link(glyphs, &rel, &sel))
        })
    }

    /// Where the four numbers of a note's link land on the page today, as quads to paint.
    pub fn link_quads(&self, page: u32, selection: Vec<u32>) -> Answer<Vec<pdf::Rect>> {
        self.with(|s| {
            let Ok(sel): Result<[usize; 4], _> = selection
                .iter()
                .map(|n| *n as usize)
                .collect::<Vec<_>>()
                .try_into()
            else {
                return Err(AccentError::Failed {
                    reason: "a selection is four numbers".to_string(),
                });
            };
            let glyphs = s.page_glyphs(page as usize)?;
            Ok(pdf::selection_quads(glyphs, sel)
                .map(|(_, quads)| quads)
                .unwrap_or_default())
        })
    }

    pub fn links(&self, page: u32) -> Answer<Vec<PdfLinkBox>> {
        self.with(|s| Ok(convert::all(s.doc.links(page as usize)?)))
    }

    pub fn outline(&self) -> Answer<Vec<Outline>> {
        self.with(|s| Ok(convert::all(s.doc.outline()?)))
    }

    /// Every hit of `text` on one page, each as the lines it runs over. One page per call, so a
    /// caller walking a long document can stop between them.
    pub fn search(&self, page: u32, text: String) -> Answer<Vec<SearchLine>> {
        self.with(|s| Ok(convert::all(s.doc.search(page as usize, &text)?)))
    }

    /// The `/Highlight` annotations the file itself carries, which are the exported ones.
    pub fn highlights(&self, page: u32) -> Answer<Vec<Highlight>> {
        self.with(|s| Ok(convert::all(s.doc.highlights_on(page as usize)?)))
    }

    // -------------------------------------------------------------------------------- drawing

    /// Draw a stroke through `points`, in page points. How much of the page that changed.
    pub fn add_stroke(&self, page: u32, points: Vec<Point>, style: InkStyle) -> Answer<pdf::Rect> {
        self.with(|s| {
            let page = page as usize;
            let points: Vec<(f32, f32)> = points.into_iter().map(Into::into).collect();
            let before = s.doc.annotation_count(page)?;
            let area = s.doc.add_ink(page, &points, style.into())?;
            s.ink.drew(page, before);
            s.ink.dirty = true;
            s.inks.remove(&page);
            Ok(area)
        })
    }

    /// Erase along the line from `from` to `to`: the first stroke it crosses, whole or cut in
    /// two, depending on `partial`.
    ///
    /// One stroke per call, so a drag calls until it gets `None` back. `joined` folds the call
    /// into the gesture before it, which is what makes one drag one Undo: false for the first
    /// hit of a drag, true after.
    pub fn erase_at(
        &self,
        page: u32,
        from: Point,
        to: Point,
        radius: f32,
        partial: bool,
        joined: bool,
    ) -> Answer<Option<pdf::Rect>> {
        self.with(|s| {
            let page = page as usize;
            let (a, b) = (from.into(), to.into());
            let hit = s
                .page_inks(page)?
                .iter()
                .find(|(_, shape)| {
                    pdf::swept(&shape.points, a, b, radius + shape.style.width / 2.0)
                })
                .map(|(id, shape)| (*id, shape.cuttable));
            let Some((id, cuttable)) = hit else {
                return Ok(None);
            };
            let Some(index) = s.ink.index_of(page, id) else {
                return Ok(None);
            };
            // A stroke another editor drew in a space of its own is taken whole rather than cut
            // into the wrong place; see `InkShape::cuttable`.
            let cut = match partial && cuttable {
                true => s.doc.cut_ink(page, index, a, b, radius)?,
                false => None,
            };
            let area = match cut {
                Some(cut) => {
                    let area = cut.area;
                    let left: Vec<_> = cut
                        .left
                        .into_iter()
                        .map(|piece| (pdf::fresh_id(), piece))
                        .collect();
                    s.ink.erased(page, index, cut.was, left, joined);
                    area
                }
                None => {
                    let (was, area) = s.doc.take_ink(page, index)?;
                    s.ink.erased(page, index, was, Vec::new(), joined);
                    area
                }
            };
            s.ink.dirty = true;
            s.inks.remove(&page);
            Ok(Some(area))
        })
    }

    /// Walk the last gesture back, and say which page each of its steps changed and how much.
    pub fn undo(&self) -> Vec<PageArea> {
        self.walk(false)
    }

    /// Walk the last gesture Undo took back forward again.
    pub fn redo(&self) -> Vec<PageArea> {
        self.walk(true)
    }

    /// Whether Undo, and then Redo, has anything to walk.
    pub fn history(&self) -> History {
        let s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let (undo, redo) = s.ink.history();
        History { undo, redo }
    }

    /// Whether anything has been drawn or erased since the last save.
    pub fn dirty(&self) -> bool {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).ink.dirty
    }

    // ---------------------------------------------------------------------------------- saving

    /// Write the document back where it was opened from, refusing if it has changed under us.
    ///
    /// `None` when there was nothing to write. A document opened from bytes has no file to write
    /// to and fails here; use [`save_bytes`](Self::save_bytes).
    pub fn save(&self) -> Answer<Option<Etag>> {
        self.with(|s| {
            if !s.ink.dirty {
                return Ok(None);
            }
            let Some(path) = s.path.clone() else {
                return Err(AccentError::Failed {
                    reason: "this document was opened from bytes and has no file to save into"
                        .to_string(),
                });
            };
            let bytes = s.doc.save()?;
            let etag = fs::write_bytes(&path, &bytes, s.etag)?;
            s.etag = Some(etag);
            s.ink.dirty = false;
            Ok(Some(etag))
        })
    }

    /// The whole document as bytes, for a caller that has to write it somewhere itself.
    pub fn save_bytes(&self) -> Answer<Vec<u8>> {
        self.with(|s| {
            let bytes = s.doc.save()?;
            s.ink.dirty = false;
            Ok(bytes)
        })
    }
}

impl PdfSession {
    fn walk(&self, forwards: bool) -> Vec<PageArea> {
        let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let State { doc, ink, inks, .. } = &mut *s;
        let changed = ink.walk(doc, forwards);
        for (page, _) in &changed {
            inks.remove(page);
        }
        if !changed.is_empty() {
            ink.dirty = true;
        }
        convert::all(changed)
    }
}

impl State {
    fn page_glyphs(&mut self, page: usize) -> Result<&[pdf::Glyph], AccentError> {
        if !self.glyphs.contains_key(&page) {
            self.glyphs.insert(page, self.doc.page_text(page)?);
        }
        Ok(&self.glyphs[&page])
    }

    /// Every ink stroke of a page under the id the tools name it by, listed once per change.
    fn page_inks(&mut self, page: usize) -> Result<&[NamedInk], AccentError> {
        if !self.inks.contains_key(&page) {
            let named = self.ink.named(&self.doc, page);
            self.inks.insert(page, named);
        }
        Ok(&self.inks[&page])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("accent-ffi-{name}-{}.pdf", std::process::id()))
    }

    fn pen() -> InkStyle {
        InkStyle {
            width: 2.0,
            rgba: 0x0000_00ff,
            multiply: false,
        }
    }

    fn line() -> Vec<Point> {
        (0..5)
            .map(|i| Point {
                x: 20.0 + i as f32 * 40.0,
                y: 50.0,
            })
            .collect()
    }

    /// The whole pen round trip as Android drives it: draw, undo, redo, erase, then save through
    /// the etag gate — and a save made against a stamp the file has moved past is refused.
    #[test]
    fn a_stroke_is_drawn_undone_erased_and_saved_against_its_etag() {
        if !pdf::available() {
            eprintln!("skipping: no libpdfium");
            return;
        }
        let path = scratch("pen");
        std::fs::write(&path, pdf::blank_pdf().unwrap()).unwrap();
        let s = PdfSession::open(path.to_string_lossy().into_owned()).unwrap();
        assert_eq!(s.page_count(), 1);
        assert!(!s.dirty());

        s.add_stroke(0, line(), pen()).unwrap();
        assert!(s.dirty());
        assert!(s.history().undo);
        assert_eq!(s.undo().len(), 1);
        assert_eq!(
            s.history(),
            History {
                undo: false,
                redo: true
            }
        );
        assert_eq!(s.redo().len(), 1);

        // A miss leaves the page alone; a pass across the stroke takes it.
        let away = (Point { x: 5.0, y: 5.0 }, Point { x: 6.0, y: 6.0 });
        assert!(
            s.erase_at(0, away.0, away.1, 4.0, false, false)
                .unwrap()
                .is_none()
        );
        let across = (Point { x: 100.0, y: 20.0 }, Point { x: 100.0, y: 80.0 });
        assert!(
            s.erase_at(0, across.0, across.1, 4.0, false, false)
                .unwrap()
                .is_some(),
            "the pass crossed the stroke"
        );

        let etag = s.save().unwrap().expect("there was something to write");
        assert!(!s.dirty());
        assert!(s.save().unwrap().is_none(), "nothing left to write");

        // The file moves under the session, and the next save is refused rather than clobbering.
        s.add_stroke(0, line(), pen()).unwrap();
        std::fs::write(&path, pdf::blank_pdf().unwrap()).unwrap();
        assert_ne!(accent_core::fs::Etag::of(&path).unwrap(), etag);
        assert!(
            matches!(s.save(), Err(AccentError::ChangedOnDisk { .. })),
            "a save against a stale stamp must be refused"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// A document with no file behind it draws and hands its bytes back, and never writes.
    #[test]
    fn a_document_opened_from_bytes_saves_only_as_bytes() {
        if !pdf::available() {
            eprintln!("skipping: no libpdfium");
            return;
        }
        let s = PdfSession::open_bytes(pdf::blank_pdf().unwrap()).unwrap();
        s.add_stroke(0, line(), pen()).unwrap();
        assert!(matches!(s.save(), Err(AccentError::Failed { .. })));
        let bytes = s.save_bytes().unwrap();
        assert!(bytes.starts_with(b"%PDF"), "not a pdf");
        assert!(!s.dirty());
        // And the bytes it gave back really carry the stroke.
        let again = PdfSession::open_bytes(bytes).unwrap();
        assert_eq!(again.0.lock().unwrap().doc.inks(0).unwrap().len(), 1);
    }
}
