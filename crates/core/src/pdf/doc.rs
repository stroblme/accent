//! The document: opening, page sizes, rendering, text, links, the outline, search and saving.
//!
//! pdfium reads the document and never writes it. An edit goes into the session's update
//! ([`Objects`]), and pdfium is opened again over the file with that update after it ([`Tail`]),
//! which is also what a save writes: the file's bytes as they were read, then one section.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use super::cos::Objects;
use super::refused;
use crate::{Error, Result};
use accent_pdf::{Pdf, Ref, Source};
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
    /// The file the document was read from, and what it looked like when it was opened; `None`
    /// for a document read from bytes. See [`PdfDoc::intact`].
    file: Option<(Arc<File>, Etag)>,
    /// What the session's update follows: that file, or bytes — Android's, or a damaged file as
    /// pdfium repaired it, which is then `repaired` and saved whole.
    base: Arc<dyn Source>,
    base_len: u64,
    repaired: bool,
    pub(super) cos: Objects,
    /// The update as pdfium was last opened over it, which a save writes after the base.
    section: Arc<Vec<u8>>,
}

impl Drop for PdfDoc {
    fn drop(&mut self) {
        let _guard = lock();
        drop(self.doc.take());
    }
}

/// What pdfium reads: the base's first `base_len` bytes, then the session's section. The base is
/// read at offsets (`pread`), so no file position is shared with anything else reading it.
struct Tail {
    base: Arc<dyn Source>,
    base_len: u64,
    section: Arc<Vec<u8>>,
    at: u64,
}

impl Read for Tail {
    /// As much of `buf` as there is: pdfium takes a short read of a block for the whole of it.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut done = 0;
        while done < buf.len() {
            let n = match self.at.checked_sub(self.base_len) {
                None => {
                    let want = (buf.len() - done).min((self.base_len - self.at) as usize);
                    self.base.read_at(self.at, &mut buf[done..done + want])?
                }
                Some(from) => {
                    let rest = self.section.get(from as usize..).unwrap_or_default();
                    let n = rest.len().min(buf.len() - done);
                    buf[done..done + n].copy_from_slice(&rest[..n]);
                    n
                }
            };
            if n == 0 {
                break;
            }
            self.at += n as u64;
            done += n;
        }
        Ok(done)
    }
}

impl Seek for Tail {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let len = self.base_len + self.section.len() as u64;
        let at = match to {
            SeekFrom::Start(at) => Some(at),
            SeekFrom::End(by) => len.checked_add_signed(by),
            SeekFrom::Current(by) => self.at.checked_add_signed(by),
        };
        self.at = at.ok_or(io::ErrorKind::InvalidInput)?;
        Ok(self.at)
    }
}

/// The objects of `base` and its pages, as `accent_pdf` reads them.
fn objects_of(base: &Arc<dyn Source>) -> accent_pdf::Result<(Pdf, Vec<Ref>)> {
    let pdf = Pdf::open(base.clone())?;
    let pages = pdf.pages()?;
    Ok((pdf, pages))
}

impl PdfDoc {
    pub fn open(path: impl AsRef<Path>) -> Result<PdfDoc> {
        let path = path.as_ref();
        PdfDoc::from_file(File::open(path).map_err(|e| Error::io(path.display(), e))?)
    }

    /// A document read from a file already open, which is read as it is needed for as long as
    /// the document lives.
    pub(super) fn from_file(file: File) -> Result<PdfDoc> {
        let read = Etag::from_meta(&file.metadata().map_err(|e| Error::io("the pdf", e))?);
        let file = Arc::new(file);
        PdfDoc::over(file.clone(), Some((file, read)))
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
    /// URI, where there is no file to open.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<PdfDoc> {
        PdfDoc::over(Arc::new(bytes), None)
    }

    /// The document `base` holds, read by pdfium and by `accent_pdf`.
    ///
    /// A file `accent_pdf` cannot read as pdfium does — an xref that does not parse, an object
    /// that is not where the xref says, a page tree that loops or counts other pages — is nothing
    /// to append to. pdfium, which repairs what it reads, writes it out once, before it
    /// has rendered anything into it, and those bytes are the base: the first save writes them
    /// whole, and the file saved is sound for the next session to append to.
    fn over(base: Arc<dyn Source>, file: Option<(Arc<File>, Etag)>) -> Result<PdfDoc> {
        let pdfium = pdfium()?;
        let read = objects_of(&base);
        let (doc, base, (pdf, pages), repaired) = {
            let _guard = lock();
            let tail = Tail {
                base: base.clone(),
                base_len: base.size(),
                section: Arc::default(),
                at: 0,
            };
            // Declared after the guard, so an early return closes it under the lock.
            let doc = pdfium
                .load_pdf_from_reader(tail, None)
                .map_err(refused("open pdf"))?;
            let count = doc.pages().len() as usize;
            match read {
                Ok((pdf, pages)) if pages.len() == count => (doc, base, (pdf, pages), false),
                _ => {
                    let bytes = doc.save_to_bytes().map_err(refused("repair pdf"))?;
                    let repaired: Arc<dyn Source> = Arc::new(bytes);
                    let read = objects_of(&repaired)?;
                    (doc, repaired, read, true)
                }
            }
        };
        let mut made = PdfDoc {
            doc: Some(doc),
            file,
            base_len: base.size(),
            base,
            repaired,
            cos: Objects::new(pdf, pages),
            section: Arc::default(),
        };
        if repaired {
            made.reopen()?;
        }
        Ok(made)
    }

    /// pdfium opened again over the base and the session's update as it now stands: what every
    /// edit ends in. Whatever pdfium generated into the document it closes goes with it.
    pub(super) fn reopen(&mut self) -> Result<()> {
        let section = Arc::new(match self.cos.update.is_empty() {
            true => Vec::new(),
            false => self.cos.update.section(),
        });
        let tail = Tail {
            base: self.base.clone(),
            base_len: self.base_len,
            section: section.clone(),
            at: 0,
        };
        let pdfium = pdfium()?;
        let _guard = lock();
        let doc = pdfium
            .load_pdf_from_reader(tail, None)
            .map_err(refused("read the edited pdf"))?;
        // The document it replaces is closed here, under the lock.
        self.doc = Some(doc);
        self.section = section;
        Ok(())
    }

    pub fn page_count(&self) -> usize {
        let _guard = lock();
        self.doc().pages().len() as usize
    }

    pub(super) fn doc(&self) -> &PdfDocument<'static> {
        self.doc.as_ref().expect("document is closed only in Drop")
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
            let bitmap = p
                .render_with_config(&config)
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
            p.render_into_bitmap_with_config(&mut bitmap, &config)
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

    /// The page's text as one string, as pdfium reads it out (`FPDFText_GetText`): the text its
    /// [`search`](Self::search) runs over, which is what the index holds of a PDF.
    pub fn page_plain_text(&self, page: usize) -> Result<String> {
        let _guard = lock();
        let text = self
            .page(page)?
            .text()
            .map_err(|e| Error::Pdf(format!("text page {page}: {e:?}")))?
            .all();
        Ok(text)
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

    /// The document as it now stands, as the bytes of a file: the base as it was read, then the
    /// session's section. What Android's `save_bytes` and a print copy hand on.
    pub fn save(&self) -> Result<Vec<u8>> {
        let mut bytes = self.base_bytes()?;
        bytes.extend_from_slice(&self.section);
        Ok(bytes)
    }

    /// The base's bytes as they were read, which every save of the session starts with.
    pub(super) fn base_bytes(&self) -> Result<Vec<u8>> {
        let bytes =
            (self.base.read_vec(0, self.base_len as usize)).map_err(|e| Error::io("the pdf", e))?;
        match bytes.len() as u64 == self.base_len {
            true => Ok(bytes),
            false => Err(Error::Pdf(
                "the pdf is shorter than when it was read".to_string(),
            )),
        }
    }

    /// Write the document to `path` as [`crate::fs::write_bytes`] writes a file, gated on
    /// `expected`: the file it was read from is copied as it is (a reflink where the file system
    /// has them) and the section written after it, so every save of a session leaves one update
    /// after the original bytes. A repaired file, or one read from bytes, is written whole.
    pub fn save_to(&self, path: &Path, expected: Option<Etag>) -> Result<Etag> {
        match (&self.file, self.repaired) {
            (Some((file, _)), false) => {
                crate::fs::write_appended(path, file, self.base_len, &self.section, expected)
            }
            _ => crate::fs::write_bytes(path, &self.save()?, expected),
        }
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
    Ok(accent_pdf::new_document(size))
}
