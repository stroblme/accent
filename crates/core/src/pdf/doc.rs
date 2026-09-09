//! The document: opening, page sizes, rendering, text, links, the outline, search and saving.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use pdfium_render::prelude::*;

use super::{Glyph, Link, LinkTarget, Outline, Rect, RgbaImage, Theme, lock, pdfium, recolour};

pub struct PdfDoc {
    // `Option` only so that `Drop` can close the document while still holding `CALLS`; it is
    // `Some` for the whole life of the value.
    doc: Option<PdfDocument<'static>>,
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
        let pdfium = pdfium()?;
        let _guard = lock();
        let doc = pdfium
            .load_pdf_from_file(path, None)
            .with_context(|| format!("open pdf {}", path.display()))?;
        Ok(PdfDoc { doc: Some(doc) })
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
            .map_err(|e| anyhow!("page {page}: {e:?}"))
    }

    /// `(width, height)` in points.
    /// One page's size in points, without loading the page.
    ///
    /// `FPDF_GetPageSizeByIndexF` reads the size out of the page tree; going through a loaded
    /// `PdfPage` instead costs a full parse of that page's content, which for a 1 500-page
    /// document is eleven seconds of work to learn how big the paper is.
    pub fn page_size(&self, page: usize) -> Result<(f32, f32)> {
        let _guard = lock();
        let rect = self
            .doc()
            .pages()
            .page_size(page as PdfPageIndex)
            .map_err(|e| anyhow!("page {page}: {e:?}"))?;
        Ok((rect.width().value, rect.height().value))
    }

    /// Every page's size in points, in one pass and without loading a single page.
    pub fn page_sizes(&self) -> Result<Vec<(f32, f32)>> {
        let _guard = lock();
        let sizes = self
            .doc()
            .pages()
            .page_sizes()
            .map_err(|e| anyhow!("page sizes: {e:?}"))?;
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
                .map_err(|e| anyhow!("render page {page}: {e:?}"))?;
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
                return Err(anyhow!(
                    "tile ({x},{y}) {w}x{h} is outside page {page} at {page_w}x{page_h} px"
                ));
            }
            // A negative origin shifts the page up and left; the render is clipped to the bitmap.
            let config = PdfRenderConfig::new()
                .scale_page_by_factor(scale)
                .set_origin(-x, -y)
                .render_annotations(true);
            let mut bitmap = PdfBitmap::empty(w, h, PdfBitmapFormat::BGRA)
                .map_err(|e| anyhow!("tile bitmap {w}x{h}: {e:?}"))?;
            p.render_into_bitmap_with_config(&mut bitmap, &config)
                .map_err(|e| anyhow!("render tile of page {page}: {e:?}"))?;
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
        let text = p.text().map_err(|e| anyhow!("text page {page}: {e:?}"))?;
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

    /// Text inside a rectangle, empty if the page or region has none.
    pub fn text_in_rect(&self, page: usize, rect: Rect) -> String {
        let _guard = lock();
        let Ok(p) = self.page(page) else {
            return String::new();
        };
        let page_height = p.height().value;
        let Ok(text) = p.text() else {
            return String::new();
        };
        text.inside_rect(rect.to_pdf(page_height))
    }

    /// The document as it now stands, annotations included.
    ///
    // ponytail: `FPDF_SaveAsCopy` rewrites the whole file, so a 100 MB PDF is 100 MB of work
    // under the global pdfium lock and loses whatever incremental history the file had. Saving
    // incrementally (`FPDF_INCREMENTAL`) is the upgrade if that ever bites.
    pub fn save(&self) -> Result<Vec<u8>> {
        let _guard = lock();
        self.doc().save_to_bytes().context("save pdf")
    }

    /// The `/Link` annotations on one page, in document order. Links whose target we cannot
    /// resolve to a page or a URI are skipped.
    ///
    /// Deliberately **not** `PdfPage::links()`. `PdfPageLinks::get(i)` treats `i` as a link index,
    /// but passes it to `FPDFLink_Enumerate` as a start position in the page's `/Annots` array,
    /// and that function scans *forward* from there to the next `/Link`. So every non-link
    /// annotation before a link makes that link answer one more index: on our own two-page
    /// fixture, whose first page is a highlight followed by a link, `links()` reports two links
    /// and returns the same one for both. Filtering `annotations()` is exact and no more code, so
    /// do not "simplify" this back.
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

    /// Text matches on one page: one entry per match, one rect per line the match spans.
    /// Case-insensitive; an empty query finds nothing.
    pub fn search(&self, page: usize, query: &str) -> Result<Vec<Vec<Rect>>> {
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let _guard = lock();
        let p = self.page(page)?;
        let page_height = p.height().value;
        let text = p.text().map_err(|e| anyhow!("text page {page}: {e:?}"))?;
        let search = text
            .search(query, &PdfSearchOptions::new())
            .map_err(|e| anyhow!("search page {page}: {e:?}"))?;
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

/// A blank single-page A4 document, for a sketch a note wants to draw on.
///
/// Portrait, because a note reads top-down and that is the shape that embeds in its flow; Fit
/// Width shows the whole page either way. Landscape would be a preference, not a default.
pub fn blank_pdf() -> Result<Vec<u8>> {
    let pdfium = pdfium()?;
    let _guard = lock();
    let mut doc = pdfium.create_new_pdf().context("create pdf")?;
    doc.pages_mut()
        .create_page_at_end(PdfPagePaperSize::a4())
        .context("create page")?;
    let bytes = doc.save_to_bytes().context("save new pdf")?;
    // Closed here rather than at the end of the function, so it happens under the lock like
    // every other document this module drops.
    drop(doc);
    Ok(bytes)
}
