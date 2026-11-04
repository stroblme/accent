//! The document: opening, page sizes, rendering, text, links, the outline, search and saving.
//!
//! pdfium renders the document and reads its text, and never writes it; `accent_pdf` reads its
//! links, outline and information dictionary. An edit goes into the session's update
//! ([`Objects`]), and pdfium is opened again over the file with that update after it ([`Tail`]),
//! which is also what a save writes: the file's bytes as they were read, then one section.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};

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
    /// The last page rendered, retaining its parsed paths and decoded images across tiles.
    /// Closed under CALLS before the document is replaced or dropped.
    rendered: Mutex<Option<(usize, PdfPage<'static>)>>,
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
        self.rendered
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take();
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
            rendered: Mutex::new(None),
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
        self.rendered
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take();
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

    /// Keep one parsed page across renders, under the same lock as every pdfium operation.
    /// Other readers load their own pages without evicting the one being drawn.
    fn rendering<T>(&self, page: usize, draw: impl FnOnce(&PdfPage<'_>) -> Result<T>) -> Result<T> {
        let _guard = lock();
        let mut rendered = self.rendered.lock().unwrap_or_else(|e| e.into_inner());
        if rendered.as_ref().is_none_or(|(at, _)| *at != page) {
            // Release a large page before loading another one.
            rendered.take();
            let loaded = self
                .doc()
                .pages()
                .get(page as PdfPageIndex)
                .map_err(|e| Error::Pdf(format!("page {page}: {e:?}")))?;
            *rendered = Some((page, loaded));
        }
        draw(&rendered.as_ref().expect("page loaded above").1)
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
        let (width, height, mut data) = self.rendering(page, |p| {
            let config = PdfRenderConfig::new()
                .scale_page_by_factor(scale)
                .limit_render_image_cache_size(true)
                .render_annotations(true);
            let bitmap = p
                .render_with_config(&config)
                .map_err(|e| Error::Pdf(format!("render page {page}: {e:?}")))?;
            Ok((
                bitmap.width() as u32,
                bitmap.height() as u32,
                bitmap.as_rgba_bytes(),
            ))
        })?;
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
        let (width, height, mut data) = self.rendering(page, |p| {
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
                .limit_render_image_cache_size(true)
                .render_annotations(true);
            let mut bitmap = PdfBitmap::empty(w, h, PdfBitmapFormat::BGRA)
                .map_err(|e| Error::Pdf(format!("tile bitmap {w}x{h}: {e:?}")))?;
            p.render_into_bitmap_with_config(&mut bitmap, &config)
                .map_err(|e| Error::Pdf(format!("render tile of page {page}: {e:?}")))?;
            Ok((w as u32, h as u32, bitmap.as_rgba_bytes()))
        })?;
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

    /// The document as `accent_pdf` reads it now: the file, with the session's update over it.
    fn document(&self) -> accent_pdf::Document<'_> {
        accent_pdf::Document::new(&self.cos.pdf, Some(&self.cos.update), &self.cos.pages)
    }

    /// What `read` makes of the document; where an object is not where the xref puts it, what
    /// it makes of the document's bytes read through an xref rebuilt from them, as pdfium
    /// repairs a file.
    fn reading<T>(
        &self,
        read: impl Fn(&accent_pdf::Document) -> accent_pdf::Result<T>,
    ) -> Result<T> {
        match read(&self.document()) {
            Err(accent_pdf::Error::Broken(_) | accent_pdf::Error::Syntax { .. }) => {
                let pdf = Pdf::rebuilt(Arc::new(self.save()?))?;
                Ok(read(&accent_pdf::Document::new(
                    &pdf,
                    None,
                    &self.cos.pages,
                ))?)
            }
            read => Ok(read?),
        }
    }

    /// The `/Link` annotations of a page that lead to a page of the document or to a URI, in
    /// `/Annots` order.
    pub fn links(&self, page: usize) -> Result<Vec<Link>> {
        let at = self.cos.page(page)?;
        let links = self.reading(|doc| doc.links(at))?;
        let (_, height) = self.page_size(page)?;
        // A destination's top is in its own page's space, so the page it names gives its height.
        let height_of = |page| self.page_size(page).map_or(height, |(_, h)| h);
        Ok(links
            .into_iter()
            .map(|link| {
                let [left, bottom, right, top] = link.rect;
                Link {
                    rect: Rect {
                        left,
                        top: height - top,
                        right,
                        bottom: height - bottom,
                    },
                    target: match link.target {
                        accent_pdf::Target::Page(dest) => LinkTarget::Page {
                            page: dest.page,
                            top: dest.top.map(|y| height_of(dest.page) - y),
                        },
                        accent_pdf::Target::Uri(uri) => LinkTarget::Uri(uri),
                    },
                }
            })
            .collect())
    }

    /// The document outline (`/Outlines`), flattened depth-first.
    pub fn outline(&self) -> Result<Vec<Outline>> {
        let outline = self.reading(|doc| doc.outline())?;
        Ok(outline
            .into_iter()
            .map(|b| Outline {
                depth: b.depth,
                title: b.title,
                page: b.page,
            })
            .collect())
    }

    /// What the document says about itself, blank entries left out (`Info`).
    pub fn info(&self) -> Info {
        let info = self.reading(|doc| doc.info()).unwrap_or_else(|e| {
            tracing::debug!("reading the information dictionary: {e:#}");
            Default::default()
        });
        Info {
            title: info.title,
            author: info.author,
            subject: info.subject,
            keywords: info.keywords,
            creator: info.creator,
            producer: info.producer,
            created: info.created.as_deref().and_then(parse_date),
            modified: info.modified.as_deref().and_then(parse_date),
            version: info.version,
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
