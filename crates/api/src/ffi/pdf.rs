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
    self, Glyph, Highlight, History, InkStyle, LinkHighlight, Located, Outline, PageArea, PageSize,
    PdfLink, PdfLinkBox, Point, SearchLine, Theme, Tile,
};
use crate::ffi::error::{AccentError, Answer};

/// A selection link with the selected text as its alias, which is what Copy Link puts on the
/// clipboard: `[[f.pdf#page=1&selection=…|the text]]`. See [`pdf::link_with_alias`].
#[uniffi::export]
pub fn link_with_alias(link: String, text: String) -> String {
    pdf::link_with_alias(&link, &text)
}

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

    /// The glyphs the four numbers of a link cover on the page today, which is what following
    /// one shows as the selection. `None` when they no longer fit the page's lines.
    pub fn locate(&self, page: u32, selection: Vec<u32>) -> Answer<Option<Located>> {
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
            Ok(
                pdf::selection_quads(glyphs, sel).map(|(range, quads)| Located {
                    start: range.start as u32,
                    end: range.end as u32,
                    quads,
                }),
            )
        })
    }

    /// Where the note links that highlight this document land today, by the desktop's rules
    /// ([`pdf::highlight_quads`]), in page order. `link` is the index into `links`, so a caller
    /// may hand over one page's links or all of them; a link whose selection is not four numbers
    /// lands nowhere.
    pub fn link_highlights(&self, links: Vec<PdfLink>) -> Vec<LinkHighlight> {
        let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let State { doc, glyphs, .. } = &mut *s;
        let (at, links): (Vec<usize>, Vec<_>) = links
            .into_iter()
            .enumerate()
            .filter_map(|(i, l)| Some((i, l.try_into().ok()?)))
            .unzip();
        let mut found: Vec<_> = pdf::highlight_quads(doc, glyphs, &links)
            .into_iter()
            .collect();
        found.sort_by_key(|(page, _)| *page);
        found
            .into_iter()
            .flat_map(|(page, quads)| quads.into_iter().map(move |(quads, i)| (page, quads, i)))
            .map(|(page, quads, i)| LinkHighlight {
                page: page as u32,
                quads,
                link: at[i] as u32,
            })
            .collect()
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
        self.with(|s| {
            let found = s.doc.search(page as usize, &text, Default::default())?;
            Ok(convert::all(found))
        })
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
        // Only the desktop edits pages, so the history here holds ink alone.
        let changed: Vec<(usize, pdf::Rect)> = ink
            .walk(doc, forwards)
            .into_iter()
            .filter_map(|walked| match walked {
                pdf::Walked::Ink(page, area) => Some((page, area)),
                pdf::Walked::Pages(..) => None,
            })
            .collect();
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
        std::fs::write(&path, pdf::blank_pdf(pdf::A4).unwrap()).unwrap();
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
        std::fs::write(&path, pdf::blank_pdf(pdf::A4).unwrap()).unwrap();
        assert_ne!(accent_core::fs::Etag::of(&path).unwrap(), etag);
        assert!(
            matches!(s.save(), Err(AccentError::ChangedOnDisk { .. })),
            "a save against a stale stamp must be refused"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// One 200 x 100 pt page reading "Hello accent" on one line.
    fn text_pdf() -> Vec<u8> {
        let content = "BT /F1 24 Tf 20 40 Td (Hello accent) Tj ET";
        let objs = [
            "<</Type/Catalog/Pages 2 0 R>>".to_string(),
            "<</Type/Pages/Kids[3 0 R]/Count 1>>".to_string(),
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]\
             /Resources<</Font<</F1 4 0 R>>>>/Contents 5 0 R>>"
                .to_string(),
            "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".to_string(),
            format!("<</Length {}>>stream\n{content}\nendstream", content.len()),
        ];
        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{o}\nendobj\n", i + 1));
        }
        let xref = out.len();
        out.push_str("xref\n0 6\n0000000000 65535 f \n");
        for off in offsets {
            out.push_str(&format!("{off:010} 00000 n \n"));
        }
        out.push_str(&format!(
            "trailer\n<</Size 6/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n"
        ));
        out.into_bytes()
    }

    /// A note's links land where their numbers say and keep the index they were handed at, one
    /// that is not four numbers included; a followed link locates its glyphs, and numbers that
    /// fit no line locate nothing.
    #[test]
    fn a_note_link_lands_and_a_followed_link_is_located() {
        if !pdf::available() {
            eprintln!("skipping: no libpdfium");
            return;
        }
        let s = PdfSession::open_bytes(text_pdf()).unwrap();
        let link = |selection: Vec<u32>| PdfLink {
            src_rel_path: "Note.md".to_string(),
            byte_start: 0,
            page: 0,
            selection,
            alias: None,
        };
        let found = s.link_highlights(vec![
            link(vec![0, 0, 0, 5]),
            link(vec![1, 2]),
            link(vec![0, 6, 0, 12]),
        ]);
        let at: Vec<u32> = found.iter().map(|h| h.link).collect();
        assert_eq!(at, [0, 2]);
        assert!(found.iter().all(|h| h.page == 0 && !h.quads.is_empty()));

        let hello = s.locate(0, vec![0, 0, 0, 5]).unwrap().expect("on the page");
        assert_eq!((hello.start, hello.end), (0, 5));
        // And what Copy Link makes of those glyphs is the desktop's link, quoting them.
        let copied = s.selection_link("a.pdf".to_string(), 0, 0, 5).unwrap();
        assert_eq!(
            link_with_alias(copied.link, copied.text),
            "[[a.pdf#page=1&selection=0,0,0,5|Hello]]"
        );
        assert!(s.locate(0, vec![9, 0, 9, 1]).unwrap().is_none());
        assert!(s.locate(0, vec![1, 2]).is_err());
    }

    /// A document with no file behind it draws and hands its bytes back, and never writes.
    #[test]
    fn a_document_opened_from_bytes_saves_only_as_bytes() {
        if !pdf::available() {
            eprintln!("skipping: no libpdfium");
            return;
        }
        let s = PdfSession::open_bytes(pdf::blank_pdf(pdf::A4).unwrap()).unwrap();
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
