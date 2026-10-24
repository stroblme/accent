//! The document: opening, page sizes, rendering, text, links, the outline, search and saving.

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{annot, refused};
use crate::{Error, Result};
use pdfium_render::prelude::*;

use super::{
    Glyph, Info, Link, LinkTarget, Outline, Rect, RgbaImage, Theme, lock, parse_date, pdfium,
};
use crate::fs::Etag;
use crate::recolour::recolour;
use crate::search::Options;

pub struct PdfDoc {
    // `Option` only so that `Drop` can close the document while still holding `CALLS`; it is
    // `Some` for the whole life of the value.
    doc: Option<PdfDocument<'static>>,
    /// A second handle on the file pdfium reads the document from, and what that file looked
    /// like when it was opened; `None` for a document read from bytes. See [`PdfDoc::intact`].
    file: Option<(File, Etag)>,
    /// Whether a render has left something of pdfium's own in the document that
    /// [`PdfDoc::save`] has to keep out of the file (see [`PdfDoc::drawn`]).
    stray: AtomicBool,
}

impl Drop for PdfDoc {
    fn drop(&mut self) {
        let _guard = lock();
        drop(self.doc.take());
    }
}

impl PdfDoc {
    pub fn open(path: impl AsRef<Path>) -> Result<PdfDoc> {
        let path = path.as_ref();
        PdfDoc::from_file(File::open(path).map_err(|e| Error::io(path.display(), e))?)
    }

    /// A document read from a file already open, which pdfium reads as it needs it for as long as
    /// the document lives.
    pub(super) fn from_file(file: File) -> Result<PdfDoc> {
        let pdfium = pdfium()?;
        // The same open file, not the path again: a rename in between would hand back another.
        let kept = file.try_clone().map_err(|e| Error::io("the pdf", e))?;
        let read = Etag::from_meta(&kept.metadata().map_err(|e| Error::io("the pdf", e))?);
        let _guard = lock();
        let doc = pdfium
            .load_pdf_from_reader(file, None)
            .map_err(refused("open pdf"))?;
        Ok(PdfDoc {
            doc: Some(doc),
            file: Some((kept, read)),
            stray: AtomicBool::new(false),
        })
    }

    /// Whether the file this document is read from is still the one it was opened on.
    ///
    /// pdfium reads a file as it needs it, a page at a time, so a file written into in place —
    /// which is how pdflatex writes its output — leaves every page not read yet rendering blank
    /// and every search on it finding nothing. A file replaced by a rename, as every save here,
    /// Syncthing and a remote fetch replace one, leaves the document reading the file it opened.
    pub fn intact(&self) -> bool {
        self.file.as_ref().is_none_or(|(file, read)| {
            file.metadata()
                .is_ok_and(|meta| Etag::from_meta(&meta) == *read)
        })
    }

    /// A document read from bytes rather than a path: what Android gets from a `content://`
    /// URI, where there is no file to open. pdfium takes ownership of the buffer and reads it
    /// for as long as the document lives.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<PdfDoc> {
        let pdfium = pdfium()?;
        let _guard = lock();
        let doc = pdfium
            .load_pdf_from_byte_vec(bytes, None)
            .map_err(refused("open pdf from bytes"))?;
        Ok(PdfDoc {
            doc: Some(doc),
            file: None,
            stray: AtomicBool::new(false),
        })
    }

    pub fn page_count(&self) -> usize {
        let _guard = lock();
        self.doc().pages().len() as usize
    }

    pub(super) fn doc(&self) -> &PdfDocument<'static> {
        self.doc.as_ref().expect("document is closed only in Drop")
    }

    pub(super) fn doc_mut(&mut self) -> &mut PdfDocument<'static> {
        self.doc.as_mut().expect("document is closed only in Drop")
    }

    pub(super) fn page(&self, page: usize) -> Result<PdfPage<'_>> {
        self.doc()
            .pages()
            .get(page as PdfPageIndex)
            .map_err(|e| Error::Pdf(format!("page {page}: {e:?}")))
    }

    /// Page width and height in points without loading its content. `FPDF_GetPageSizeByIndexF`
    /// reads the page tree, avoiding a costly parse for every page.
    pub fn page_size(&self, page: usize) -> Result<(f32, f32)> {
        let _guard = lock();
        let rect = self
            .doc()
            .pages()
            .page_size(page as PdfPageIndex)
            .map_err(|e| Error::Pdf(format!("page {page}: {e:?}")))?;
        Ok((rect.width().value, rect.height().value))
    }

    /// Every page's size in points, in one pass and without loading a single page.
    pub fn page_sizes(&self) -> Result<Vec<(f32, f32)>> {
        let _guard = lock();
        let sizes = self
            .doc()
            .pages()
            .page_sizes()
            .map_err(|e| Error::Pdf(format!("page sizes: {e:?}")))?;
        Ok(sizes
            .into_iter()
            .map(|rect| (rect.width().value, rect.height().value))
            .collect())
    }

    /// Render one page at `scale` pixels per point.
    pub fn render_page(&self, page: usize, scale: f32, theme: Theme) -> Result<RgbaImage> {
        let (width, height, mut data) = {
            let _guard = lock();
            let p = self.page(page)?;
            let config = PdfRenderConfig::new()
                .scale_page_by_factor(scale)
                .render_annotations(true);
            let bitmap = self
                .drawn(&p, 0, || p.render_with_config(&config))
                .map_err(|e| Error::Pdf(format!("render page {page}: {e:?}")))?;
            (
                bitmap.width() as u32,
                bitmap.height() as u32,
                bitmap.as_rgba_bytes(),
            )
        };
        // Outside the lock on purpose: the theme pass costs about as much as the render itself
        // and touches nothing but our own buffer, so it must not block other pdfium callers.
        if let Theme::Recolour { paper, ink } = theme {
            recolour(&mut data, paper, ink);
        }
        Ok(RgbaImage {
            width,
            height,
            data,
        })
    }

    /// One tile of a page, `(x, y, w, h)` in device pixels of the page scaled to `scale` px/pt.
    ///
    /// The tile must lie inside the page: pdfium clears only the part of the destination bitmap
    /// the page covers, so pixels beyond the page edge would be whatever the freshly allocated
    /// bitmap happens to hold. `w` and `h` are therefore clamped to what is left of
    /// `round(page_size * scale)`, and an origin outside the page is an error.
    // The rectangle stays four plain arguments: the caller is a tile cache that has them loose
    // anyway, and a wrapper struct would only be unpacked again here.
    #[allow(clippy::too_many_arguments)]
    pub fn render_tile(
        &self,
        page: usize,
        scale: f32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        theme: Theme,
    ) -> Result<RgbaImage> {
        let (width, height, mut data) = {
            let _guard = lock();
            let p = self.page(page)?;
            let page_w = (p.width().value * scale).round() as i32;
            let page_h = (p.height().value * scale).round() as i32;
            let (w, h) = (w.min(page_w - x), h.min(page_h - y));
            if x < 0 || y < 0 || w <= 0 || h <= 0 {
                return Err(Error::Invalid(format!(
                    "tile ({x},{y}) {w}x{h} is outside page {page} at {page_w}x{page_h} px"
                )));
            }
            // A negative origin shifts the page up and left; the render is clipped to the bitmap.
            let config = PdfRenderConfig::new()
                .scale_page_by_factor(scale)
                .set_origin(-x, -y)
                .render_annotations(true);
            let mut bitmap = PdfBitmap::empty(w, h, PdfBitmapFormat::BGRA)
                .map_err(|e| Error::Pdf(format!("tile bitmap {w}x{h}: {e:?}")))?;
            self.drawn(&p, 0, || {
                p.render_into_bitmap_with_config(&mut bitmap, &config)
            })
            .map_err(|e| Error::Pdf(format!("render tile of page {page}: {e:?}")))?;
            (w as u32, h as u32, bitmap.as_rgba_bytes())
        };
        if let Theme::Recolour { paper, ink } = theme {
            recolour(&mut data, paper, ink);
        }
        Ok(RgbaImage {
            width,
            height,
            data,
        })
    }

    /// Every character on the page with its box, in pdfium's character order (reading order for
    /// ordinary documents).
    pub fn page_text(&self, page: usize) -> Result<Vec<Glyph>> {
        let _guard = lock();
        self.page_text_locked(page)
    }

    /// [`Self::page_text`] without taking `CALLS`, for callers that already hold it.
    fn page_text_locked(&self, page: usize) -> Result<Vec<Glyph>> {
        let p = self.page(page)?;
        let page_height = p.height().value;
        let text = p
            .text()
            .map_err(|e| Error::Pdf(format!("text page {page}: {e:?}")))?;
        let chars = text.chars();
        let mut out = Vec::with_capacity(chars.len());
        for c in chars.iter() {
            // Loose bounds include the font's ascent/descent, which is what a selection highlight
            // should cover; tight bounds are the inked extent and are the fallback for glyphs
            // pdfium synthesised (generated spaces have no outline).
            let rect = c
                .loose_bounds()
                .or_else(|_| c.tight_bounds())
                .map(|r| Rect::from_pdf(r, page_height))
                .unwrap_or(Rect::ZERO);
            out.push(Glyph {
                ch: c.unicode_char().unwrap_or(char::REPLACEMENT_CHARACTER),
                rect,
                index: c.index(),
            });
        }
        Ok(out)
    }

    /// Serialise the document with annotations.
    ///
    /// After rendering leaves pdfium-generated objects ([`PdfDoc::drawn`]), blank the
    /// placeholder key and reload the first save before saving again. The second pass omits
    /// unreferenced generated appearances and popups (ISSUES.md).
    // ponytail: `FPDF_SaveAsCopy` rewrites the whole file under the global pdfium lock (80 to
    // 140 ms for a 100 MB scanned PDF, 25 to 50 ms for a 6 MB one of 383 pages of text) and loses
    // whatever incremental history the file had. `FPDF_INCREMENTAL` is no cheaper way out: pdfium
    // keeps no record of what changed and appends every object it has parsed since the document
    // was opened, so a document read through before a stroke is saved at twice its size, and
    // grows by what was read again in every session that saves (measured 2026-09-30).
    pub fn save(&self) -> Result<Vec<u8>> {
        let _guard = lock();
        let mut bytes = self.doc().save_to_bytes().map_err(refused("save pdf"))?;
        if !self.stray.load(Ordering::Relaxed) {
            return Ok(bytes);
        }
        annot::blank_taken_back(&mut bytes);
        let saved = pdfium()?
            .load_pdf_from_byte_vec(bytes, None)
            .map_err(refused("read the saved pdf"))?;
        let bytes = saved.save_to_bytes().map_err(refused("save pdf"));
        // Closed here, under the lock, like every other document.
        drop(saved);
        bytes
    }

    /// Render `p` through `draw` with its annotations, and take back what pdfium writes into the
    /// document doing so, which is not ours to write: the appearance stream it generates, with
    /// its key `PDFIUM_HasGeneratedAP`, for an annotation that has none — another reader's sticky
    /// note, say — except the last `ours` of the page, which are ours and keep theirs. The
    /// popup it makes anew for every annotation with a note stays, referred to by nothing; both
    /// are for [`PdfDoc::save`] to leave out. The caller holds the lock.
    pub(super) fn drawn<T>(&self, p: &PdfPage<'_>, ours: usize, draw: impl FnOnce() -> T) -> T {
        let noted = !self.stray.load(Ordering::Relaxed)
            && (p.annotations().iter()).any(|a| a.contents().is_some_and(|c| !c.is_empty()));
        let count = p.annotations().len();
        let bare: Vec<(usize, bool)> = (annot::bare(p).into_iter())
            .filter(|&(index, _)| index + ours < count)
            .collect();
        let drawn = draw();
        if annot::unbare(p, &bare) || noted {
            self.stray.store(true, Ordering::Relaxed);
        }
        drawn
    }

    /// List resolvable `/Link` annotations in document order.
    ///
    /// Filter `annotations()` rather than using `PdfPage::links()`: its index is passed as an
    /// `/Annots` start position, so preceding non-link annotations cause duplicate links.
    pub fn links(&self, page: usize) -> Result<Vec<Link>> {
        let _guard = lock();
        let p = self.page(page)?;
        let page_height = p.height().value;
        // Destination tops are in the target page's coordinates, so resolving one costs a page
        // load; a table of contents points at many pages, often repeatedly.
        let mut heights: HashMap<usize, f32> = HashMap::new();
        let mut out = Vec::new();
        for a in p.annotations().iter() {
            if a.annotation_type() != PdfPageAnnotationType::Link {
                continue;
            }
            let (Ok(bounds), Some(link)) = (
                a.bounds(),
                a.as_link_annotation().and_then(|l| l.link().ok()),
            ) else {
                continue;
            };
            // `destination()` already resolves a `/A` GoTo action, so it comes first.
            let target = if let Some(dest) = link.destination() {
                let Ok(target) = dest.page_index() else {
                    continue;
                };
                let target = target as usize;
                let top = dest.view_settings().ok().and_then(view_top).map(|y| {
                    let height = *heights.entry(target).or_insert_with(|| {
                        self.page(target).map_or(page_height, |p| p.height().value)
                    });
                    height - y
                });
                LinkTarget::Page { page: target, top }
            } else if let Some(uri) = link
                .action()
                .and_then(|a| a.as_uri_action().and_then(|u| u.uri().ok()))
            {
                LinkTarget::Uri(uri)
            } else {
                continue;
            };
            out.push(Link {
                rect: Rect::from_pdf(bounds, page_height),
                target,
            });
        }
        Ok(out)
    }

    /// The document outline (`/Outlines`), flattened depth-first.
    ///
    // ponytail: the walk is capped at `OUTLINE_MAX_DEPTH` levels and `OUTLINE_MAX_ENTRIES`
    // entries, because a `/Outlines` tree whose `/Next` or `/First` chain loops back on itself is
    // malformed but trivial to write, and would otherwise spin the worker forever. No real
    // document comes near either cap. The upgrade path, if one ever does, is a visited set keyed
    // by the raw `FPDF_BOOKMARK` handle, which pdfium-render keeps private today.
    pub fn outline(&self) -> Result<Vec<Outline>> {
        let _guard = lock();
        let mut out = Vec::new();
        let Some(root) = self.doc().bookmarks().root() else {
            return Ok(out);
        };
        // Pre-order: a node's sibling is pushed before its child, so the child pops first.
        let mut stack = vec![(root, 0usize)];
        while let Some((node, depth)) = stack.pop() {
            if out.len() >= OUTLINE_MAX_ENTRIES {
                break;
            }
            out.push(Outline {
                depth,
                title: node.title().unwrap_or_default(),
                page: node
                    .destination()
                    .and_then(|d| d.page_index().ok())
                    .map(|i| i as usize),
            });
            if let Some(sibling) = node.next_sibling() {
                stack.push((sibling, depth));
            }
            if depth + 1 < OUTLINE_MAX_DEPTH
                && let Some(child) = node.first_child()
            {
                stack.push((child, depth + 1));
            }
        }
        Ok(out)
    }

    /// What the document says about itself, blank entries left out (`Info`).
    pub fn info(&self) -> Info {
        use PdfDocumentMetadataTagType as Tag;
        let _guard = lock();
        let doc = self.doc();
        let tag = |kind| {
            let value = doc.metadata().get(kind)?;
            let value = value.value().trim();
            (!value.is_empty()).then(|| value.to_string())
        };
        let version = match doc.version() {
            PdfDocumentVersion::Pdf1_0 => Some("1.0"),
            PdfDocumentVersion::Pdf1_1 => Some("1.1"),
            PdfDocumentVersion::Pdf1_2 => Some("1.2"),
            PdfDocumentVersion::Pdf1_3 => Some("1.3"),
            PdfDocumentVersion::Pdf1_4 => Some("1.4"),
            PdfDocumentVersion::Pdf1_5 => Some("1.5"),
            PdfDocumentVersion::Pdf1_6 => Some("1.6"),
            PdfDocumentVersion::Pdf1_7 => Some("1.7"),
            PdfDocumentVersion::Pdf2_0 => Some("2.0"),
            PdfDocumentVersion::Unset | PdfDocumentVersion::Other(_) => None,
        };
        Info {
            title: tag(Tag::Title),
            author: tag(Tag::Author),
            subject: tag(Tag::Subject),
            keywords: tag(Tag::Keywords),
            creator: tag(Tag::Creator),
            producer: tag(Tag::Producer),
            created: tag(Tag::CreationDate).as_deref().and_then(parse_date),
            version: version.map(str::to_string),
        }
    }

    /// Text matches on one page: one entry per match, one rect per line the match spans.
    /// Without regard to case unless `options` says Match Case, and only whole words where it
    /// says Match Whole Word; the query is literal whatever it says of a regular expression,
    /// which pdfium has no way to match. An empty query finds nothing.
    pub fn search(&self, page: usize, query: &str, options: Options) -> Result<Vec<Vec<Rect>>> {
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let _guard = lock();
        let p = self.page(page)?;
        let page_height = p.height().value;
        let text = p
            .text()
            .map_err(|e| Error::Pdf(format!("text page {page}: {e:?}")))?;
        let search = text
            .search(
                query,
                &PdfSearchOptions::new()
                    .match_case(options.case)
                    .match_whole_word(options.word),
            )
            .map_err(|e| Error::Pdf(format!("search page {page}: {e:?}")))?;
        Ok(search
            .iter(PdfSearchDirection::SearchForward)
            .map(|hit| {
                hit.iter()
                    .map(|s| Rect::from_pdf(s.bounds(), page_height))
                    .collect()
            })
            .collect())
    }
}

const OUTLINE_MAX_DEPTH: usize = 32;
const OUTLINE_MAX_ENTRIES: usize = 10_000;

/// The y a destination wants at the top of the window, in pdfium's bottom-left-origin points,
/// for the view modes that carry one.
fn view_top(view: PdfDestinationViewSettings) -> Option<f32> {
    match view {
        PdfDestinationViewSettings::SpecificCoordinatesAndZoom(_, y, _)
        | PdfDestinationViewSettings::FitPageHorizontallyToWindow(y) => y.map(|y| y.value),
        PdfDestinationViewSettings::FitPageToRectangle(r) => Some(r.top().value),
        _ => None,
    }
}

/// A4 in points, portrait: the paper a blank document starts on unless another shape is picked.
pub const A4: (f32, f32) = (595.28, 841.89);

/// A blank single-page document `size` points across, for a sketch or a drawing to be made on.
///
/// Insert Sketch asks for [`A4`] portrait, because a note reads top-down and that is the shape
/// that embeds in its flow; New Drawing is where the other shapes are picked. Whatever the size,
/// the result is a PDF every reader on the machine opens and our own pen draws on.
pub fn blank_pdf(size: (f32, f32)) -> Result<Vec<u8>> {
    let pdfium = pdfium()?;
    let _guard = lock();
    let mut doc = pdfium.create_new_pdf().map_err(refused("create pdf"))?;
    doc.pages_mut()
        .create_page_at_end(paper(size))
        .map_err(refused("create page"))?;
    let bytes = doc.save_to_bytes().map_err(refused("save new pdf"))?;
    // Closed here rather than at the end of the function, so it happens under the lock like
    // every other document this module drops.
    drop(doc);
    Ok(bytes)
}

/// A size in points as pdfium's page constructors want it.
pub(super) fn paper((width, height): (f32, f32)) -> PdfPagePaperSize {
    PdfPagePaperSize::from_points(PdfPoints::new(width), PdfPoints::new(height))
}
