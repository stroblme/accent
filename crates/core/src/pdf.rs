//! PDF rendering, glyph geometry, selection links and highlight annotations, via pdfium.
//!
//! Everything here is plain data (no UI toolkit types) so the same code serves the GTK app,
//! the CLI and — later — Android through uniffi.
//!
//! Coordinates: all [`Rect`]s in this module are **page points with the origin at the top-left**
//! and y growing downwards, matching how a viewport draws them. pdfium's own coordinate space has
//! the origin at the bottom-left; the conversion happens at the boundary in [`Rect::from_pdf`] /
//! [`Rect::to_pdf`].

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use anyhow::{Context, Result, anyhow};
use pdfium_render::prelude::*;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------------------------
// Library location + global binding
// ---------------------------------------------------------------------------------------------

/// Directory that contains `libpdfium.so`.
///
/// `ACCENT_PDFIUM_DIR` overrides it; otherwise `<workspace>/vendor/pdfium`.
///
// ponytail: the default is baked in from `CARGO_MANIFEST_DIR` at build time, which is only
// correct in this checkout. The shipped app resolves the library next to the executable (desktop)
// or from the APK's `jniLibs` (Android), where the loader finds `libpdfium.so` on its own — at
// that point this default becomes "" and `bind_to_system_library()` does the work.
pub fn library_dir() -> PathBuf {
    match std::env::var_os("ACCENT_PDFIUM_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/pdfium"),
    }
}

// `Pdfium` may be initialised exactly once per process (it asserts on a second `Pdfium::new`), and
// a document borrows it. Parking it in a `OnceLock` is what buys us `PdfDocument<'static>` and
// therefore a `PdfDoc` that callers can move around freely.
static PDFIUM: OnceLock<Option<Pdfium>> = OnceLock::new();

fn pdfium() -> Result<&'static Pdfium> {
    PDFIUM
        .get_or_init(|| {
            Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(&library_dir()))
                .or_else(|_| Pdfium::bind_to_system_library())
                .map(Pdfium::new)
                .ok()
        })
        .as_ref()
        .ok_or_else(|| {
            anyhow!(
                "libpdfium not found in {} nor in the system library path \
                 (set ACCENT_PDFIUM_DIR)",
                library_dir().display()
            )
        })
}

/// Whether a usable `libpdfium` was found. Callers that can degrade (and tests) check this first.
pub fn available() -> bool {
    pdfium().is_ok()
}

// Pdfium is not thread-safe; its authors recommend parallel *processes*, not threads. Note that
// pdfium-render 0.9's `thread_safe` feature no longer serialises calls — since 0.9.0 it only adds
// `Send`/`Sync` impls, contrary to its README — so two threads touching pdfium abort the process
// with `free(): invalid size`. Every entry point below therefore holds this lock, closing a
// document included.
//
// ponytail: a global lock means page renders never overlap, so a background pre-render blocks the
// visible one. That is the ceiling. Upgrade path when it bites: give each worker its own pdfium in
// a separate process and ship bitmaps over a pipe, which is what upstream recommends anyway.
static CALLS: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    // A panic mid-call leaves pdfium's own state untouched, so a poisoned lock is still usable.
    CALLS.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------------------------
// Plain data types
// ---------------------------------------------------------------------------------------------

/// A rectangle in page points, origin top-left, `top <= bottom`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Rect {
    pub const ZERO: Rect = Rect {
        left: 0.0,
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
    };

    fn from_pdf(r: PdfRect, page_height: f32) -> Self {
        Rect {
            left: r.left().value,
            top: page_height - r.top().value,
            right: r.right().value,
            bottom: page_height - r.bottom().value,
        }
    }

    fn to_pdf(self, page_height: f32) -> PdfRect {
        PdfRect::new_from_values(
            page_height - self.bottom,
            self.left,
            page_height - self.top,
            self.right,
        )
    }

    /// Smallest rectangle containing both.
    pub fn union(self, o: Rect) -> Rect {
        Rect {
            left: self.left.min(o.left),
            top: self.top.min(o.top),
            right: self.right.max(o.right),
            bottom: self.bottom.max(o.bottom),
        }
    }

    pub fn width(&self) -> f32 {
        self.right - self.left
    }

    pub fn height(&self) -> f32 {
        self.bottom - self.top
    }

    /// The four corners in the Z-order a PDF `/QuadPoints` array expects:
    /// top-left, top-right, bottom-left, bottom-right. Still in top-left-origin points; the
    /// writer flips y when it eventually emits the annotation.
    pub fn quad_corners(&self) -> [(f32, f32); 4] {
        [
            (self.left, self.top),
            (self.right, self.top),
            (self.left, self.bottom),
            (self.right, self.bottom),
        ]
    }
}

/// A rendered page. `data` is tightly packed RGBA8, `width * height * 4` bytes.
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Theme {
    Light,
    Dark,
}

/// One character with its box on the page. `index` is pdfium's character index and is what
/// [`Selection`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Glyph {
    pub ch: char,
    pub rect: Rect,
    pub index: usize,
}

/// A range of glyph indices on one page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub page: usize,
    pub start: usize,
    pub end: usize,
}

/// An Obsidian-style selection link plus everything needed to re-anchor it ourselves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectionLink {
    pub link: String,
    pub quads: Vec<Rect>,
    pub text: String,
}

/// An existing `/Highlight` annotation read out of the document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Highlight {
    pub page: usize,
    pub quads: Vec<Rect>,
    pub color: [u8; 4],
    pub contents: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// Dark theme
// ---------------------------------------------------------------------------------------------

// Luminance the darkest input (white paper) maps to, and the brightest (black ink). Not 0.0/1.0:
// pure black behind text is harsh, and #1e1e1e matches the Adwaita dark view background.
const DARK_LO: f32 = 0.118; // 0x1e
const DARK_HI: f32 = 0.922; // 0xeb

/// Invert a pixel's lightness while keeping its chroma, so white paper becomes dark grey and
/// black text becomes light, but a yellow highlight stays yellow.
///
// ponytail: this keeps the *absolute* chroma offset `c - luminance` and clamps, which is one
// multiply-free pass over the buffer and good enough to read by. The ceiling: saturated colours
// near the ends of the ramp lose some saturation to the clamp, and it is not a real perceptual
// space. Upgrade path when someone complains is Oklab — convert, negate L, convert back — at
// roughly 3x the cost, at which point this wants SIMD or the GPU.
pub fn dark_pixel(px: [u8; 4]) -> [u8; 4] {
    let (r, g, b) = (
        px[0] as f32 / 255.0,
        px[1] as f32 / 255.0,
        px[2] as f32 / 255.0,
    );
    let l = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let l2 = DARK_LO + (1.0 - l) * (DARK_HI - DARK_LO);
    let map = |c: f32| ((l2 + (c - l)) * 255.0).clamp(0.0, 255.0) as u8;
    [map(r), map(g), map(b), px[3]]
}

// ---------------------------------------------------------------------------------------------
// Document
// ---------------------------------------------------------------------------------------------

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

    fn doc(&self) -> &PdfDocument<'static> {
        self.doc.as_ref().expect("document is closed only in Drop")
    }

    fn page(&self, page: usize) -> Result<PdfPage<'_>> {
        self.doc()
            .pages()
            .get(page as PdfPageIndex)
            .map_err(|e| anyhow!("page {page}: {e:?}"))
    }

    /// `(width, height)` in points.
    pub fn page_size(&self, page: usize) -> Result<(f32, f32)> {
        let _guard = lock();
        let p = self.page(page)?;
        Ok((p.width().value, p.height().value))
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
        if theme == Theme::Dark {
            for px in data.chunks_exact_mut(4) {
                let out = dark_pixel([px[0], px[1], px[2], px[3]]);
                px.copy_from_slice(&out);
            }
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

    /// Build an Obsidian-compatible link for a glyph range, plus the quads and text we keep for
    /// re-anchoring.
    ///
    // ponytail: Obsidian's `selection=a,b,c,d` are PDF.js text-*item* indices with a character
    // offset inside each item, and PDF.js segments a page differently from pdfium — there is no
    // way to reproduce its numbering from here, so cross-engine fidelity is best-effort: a link we
    // emit may land a few characters off when opened in Obsidian, and vice versa. That is why
    // `SelectionLink` also carries `quads` and `text`: our own viewer re-anchors from those
    // (quads first, text search as the fallback) and treats the numbers as a hint only.
    pub fn selection_link(&self, rel_pdf_path: &str, sel: &Selection) -> Result<SelectionLink> {
        let _guard = lock();
        let glyphs = self.page_text_locked(sel.page)?;
        let start = sel.start.min(glyphs.len());
        let end = sel.end.clamp(start, glyphs.len());
        let lines = line_groups(&glyphs);

        let (a, b) = item_offset(&lines, start);
        let (c, d) = item_offset(&lines, end.saturating_sub(1));

        let quads = lines
            .iter()
            .filter_map(|line| {
                let lo = line.start.max(start);
                let hi = line.end.min(end);
                (lo < hi).then(|| {
                    glyphs[lo..hi]
                        .iter()
                        .map(|g| g.rect)
                        .reduce(Rect::union)
                        .unwrap_or(Rect::ZERO)
                })
            })
            .collect();

        Ok(SelectionLink {
            // `d + 1`: PDF.js's end offset is exclusive.
            link: format!(
                "[[{rel_pdf_path}#page={}&selection={a},{b},{c},{}]]",
                sel.page + 1,
                d + 1
            ),
            quads,
            text: glyphs[start..end].iter().map(|g| g.ch).collect(),
        })
    }

    /// Existing `/Highlight` annotations across the whole document.
    pub fn highlights(&self) -> Result<Vec<Highlight>> {
        let _guard = lock();
        let mut out = Vec::new();
        for (page, p) in self.doc().pages().iter().enumerate() {
            let page_height = p.height().value;
            for a in p.annotations().iter() {
                if a.annotation_type() != PdfPageAnnotationType::Highlight {
                    continue;
                }
                let mut quads: Vec<Rect> = a
                    .attachment_points()
                    .iter()
                    .map(|q| Rect::from_pdf(q.to_rect(), page_height))
                    .collect();
                // Not every producer writes /QuadPoints; /Rect is the coarse fallback.
                if quads.is_empty()
                    && let Ok(b) = a.bounds()
                {
                    quads.push(Rect::from_pdf(b, page_height));
                }
                let c = annotation_color(&a).unwrap_or(PdfColor::new(255, 255, 0, 255));
                out.push(Highlight {
                    page,
                    quads,
                    color: [c.red(), c.green(), c.blue(), c.alpha()],
                    contents: a.contents(),
                });
            }
        }
        Ok(out)
    }
}

/// The colour an annotation is drawn in.
///
/// Deliberately avoids `PdfPageAnnotationCommon::fill_color()` **and** `stroke_color()` on any
/// annotation carrying an appearance stream. Both call `FPDFAnnot_GetColor()`, which pdfium
/// documents as returning false in exactly that case, and both then fall back to
/// `FPDFPageObj_GetFillColor(self.handle() as FPDF_PAGEOBJECT)` — casting an `FPDF_ANNOTATION`
/// to an unrelated opaque pointer type. That is UB and segfaults in practice; rendering a page
/// makes pdfium synthesise the appearance stream, so it fires on the second call and not the
/// first. Reproduced on pdfium-render 0.9.3 against both pdfium 7881 (the build its bindings are
/// generated from) and 8035, so it is a crate bug, not an ABI mismatch.
///
/// The split below keeps us on the safe branch of each: with an appearance stream we read the
/// generated path object's real fill colour, without one `FPDFAnnot_GetColor()` succeeds on `/C`
/// and the bad fallback is never reached.
///
// ponytail: the one uncovered case is an annotation whose `/AP` exists but contains no page
// objects — then `stroke_color()` still takes the crashing branch. Never seen in the wild, and
// the real fix is upstream (or our own `FPDFAnnot_GetColor` call, which needs the raw
// `FPDF_ANNOTATION` handle that pdfium-render keeps private).
fn annotation_color(a: &PdfPageAnnotation<'_>) -> Option<PdfColor> {
    if a.objects().len() > 0 {
        return a.objects().iter().find_map(|o| o.fill_color().ok());
    }
    a.stroke_color().ok()
}

// ---------------------------------------------------------------------------------------------
// Line grouping
// ---------------------------------------------------------------------------------------------

/// Split glyphs into runs that sit on one visual line.
///
// ponytail: pdfium does expose `PdfPageText::segments()`, but mapping a segment back to character
// indices goes through a fuzzy nearest-point lookup that silently drops the first/last character
// of a run. Grouping by glyph geometry is fewer lines, exact, and reused for both the quads and
// the link's item index. It assumes one column of horizontal text; rotated or multi-column pages
// get extra line breaks, which costs extra quads, never wrong ones.
fn line_groups(glyphs: &[Glyph]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..glyphs.len() {
        let (prev, cur) = (&glyphs[i - 1].rect, &glyphs[i].rect);
        let tol = prev.height().abs().max(1.0) * 0.5;
        if (cur.top - prev.top).abs() > tol || cur.left + tol < prev.left {
            out.push(start..i);
            start = i;
        }
    }
    if !glyphs.is_empty() {
        out.push(start..glyphs.len());
    }
    out
}

/// Map a glyph index to `(line index, offset within line)`.
fn item_offset(lines: &[Range<usize>], idx: usize) -> (usize, usize) {
    for (i, line) in lines.iter().enumerate() {
        if idx < line.end {
            return (i, idx.saturating_sub(line.start));
        }
    }
    match lines.last() {
        Some(line) => (lines.len() - 1, line.end - line.start),
        None => (0, 0),
    }
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-page PDF with a line of Helvetica, built by hand so the tests need no fixture.
    /// With `highlight`, the page also carries a `/Highlight` annotation over that line.
    fn tiny_pdf(highlight: bool) -> Vec<u8> {
        let content = "BT /F1 24 Tf 20 40 Td (Hello accent) Tj ET";
        let annots = if highlight { "/Annots[6 0 R]" } else { "" };
        let mut objs = vec![
            "<</Type/Catalog/Pages 2 0 R>>".to_string(),
            "<</Type/Pages/Kids[3 0 R]/Count 1>>".to_string(),
            format!(
                "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]\
                 /Resources<</Font<</F1 4 0 R>>>>/Contents 5 0 R{annots}>>"
            ),
            "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".to_string(),
            format!("<</Length {}>>stream\n{content}\nendstream", content.len()),
        ];
        if highlight {
            // /QuadPoints is in the PDF spec's order: upper-left, upper-right, lower-left,
            // lower-right, bottom-left page origin.
            objs.push(
                "<</Type/Annot/Subtype/Highlight/Rect[18 36 140 64]\
                 /QuadPoints[18 64 140 64 18 36 140 36]/C[1 1 0]/CA 1\
                 /Contents(check this)/F 4>>"
                    .to_string(),
            );
        }
        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{o}\nendobj\n", i + 1));
        }
        let xref = out.len();
        out.push_str(&format!(
            "xref\n0 {}\n0000000000 65535 f \n",
            objs.len() + 1
        ));
        for off in &offsets {
            out.push_str(&format!("{off:010} 00000 n \n"));
        }
        out.push_str(&format!(
            "trailer\n<</Size {}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        ));
        out.into_bytes()
    }

    /// Write the tiny PDF into a tempdir and open it, or `None` if pdfium is missing.
    fn open_tiny_with(highlight: bool) -> Option<(tempfile::TempDir, PdfDoc)> {
        if !available() {
            eprintln!("skipping: no libpdfium in {}", library_dir().display());
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tiny.pdf");
        std::fs::write(&path, tiny_pdf(highlight)).unwrap();
        Some((dir, PdfDoc::open(&path).unwrap()))
    }

    fn open_tiny() -> Option<(tempfile::TempDir, PdfDoc)> {
        open_tiny_with(false)
    }

    #[test]
    fn opens_and_reports_geometry() {
        let Some((_d, doc)) = open_tiny() else { return };
        assert_eq!(doc.page_count(), 1);
        let (w, h) = doc.page_size(0).unwrap();
        assert!(
            (w - 200.0).abs() < 0.5 && (h - 100.0).abs() < 0.5,
            "{w}x{h}"
        );
    }

    #[test]
    fn glyphs_are_non_empty_and_in_reading_order() {
        let Some((_d, doc)) = open_tiny() else { return };
        let glyphs = doc.page_text(0).unwrap();
        assert!(!glyphs.is_empty());
        let text: String = glyphs.iter().map(|g| g.ch).collect();
        assert!(text.contains("Hello accent"), "got {text:?}");
        // Single line of left-to-right text: boxes advance, and none is degenerate.
        let inked: Vec<_> = glyphs.iter().filter(|g| !g.ch.is_whitespace()).collect();
        assert!(inked.windows(2).all(|w| w[0].rect.left <= w[1].rect.left));
        assert!(inked.iter().all(|g| g.rect.height() > 0.0));
        // Indices are pdfium's and must line up with the vector positions we index by.
        assert!(glyphs.iter().enumerate().all(|(i, g)| g.index == i));
    }

    #[test]
    fn text_in_rect_reads_the_line() {
        let Some((_d, doc)) = open_tiny() else { return };
        let whole = Rect {
            left: 0.0,
            top: 0.0,
            right: 200.0,
            bottom: 100.0,
        };
        assert!(doc.text_in_rect(0, whole).contains("Hello"));
    }

    #[test]
    fn selection_link_has_the_obsidian_shape() {
        let Some((_d, doc)) = open_tiny() else { return };
        let sel = Selection {
            page: 0,
            start: 0,
            end: 40,
        };
        let out = doc.selection_link("notes/paper.pdf", &sel).unwrap();
        // Stand-in for ^\[\[.+\.pdf#page=\d+&selection=\d+,\d+,\d+,\d+\]\]$ without a regex dep.
        let body = out
            .link
            .strip_prefix("[[")
            .and_then(|s| s.strip_suffix("]]"))
            .expect("wrapped in [[ ]]");
        let (file, rest) = body.split_once("#page=").expect("#page=");
        assert!(file.ends_with(".pdf") && !file.is_empty());
        let (page, sel_part) = rest.split_once("&selection=").expect("&selection=");
        assert!(page.parse::<u32>().is_ok(), "page {page:?}");
        let nums: Vec<_> = sel_part.split(',').collect();
        assert_eq!(nums.len(), 4, "{sel_part:?}");
        assert!(
            nums.iter().all(|n| n.parse::<usize>().is_ok()),
            "{sel_part:?}"
        );

        assert!(out.text.starts_with("Hello"), "{:?}", out.text);
        assert!(!out.quads.is_empty());
    }

    #[test]
    fn reads_existing_highlight_annotations() {
        let Some((_d, doc)) = open_tiny_with(true) else {
            return;
        };
        let hls = doc.highlights().unwrap();
        assert_eq!(hls.len(), 1, "{hls:?}");
        let hl = &hls[0];
        assert_eq!(hl.page, 0);
        assert_eq!(hl.contents.as_deref(), Some("check this"));
        assert_eq!(hl.color, [255, 255, 0, 255], "yellow /C [1 1 0]");
        assert_eq!(hl.quads.len(), 1, "one /QuadPoints quad");
        // Page is 100pt tall; the quad spans y 36..64 bottom-up, so 36..64 top-down becomes
        // top = 100 - 64 = 36, bottom = 100 - 36 = 64.
        let q = hl.quads[0];
        assert!(
            (q.left - 18.0).abs() < 0.5 && (q.right - 140.0).abs() < 0.5,
            "{q:?}"
        );
        assert!(
            (q.top - 36.0).abs() < 0.5 && (q.bottom - 64.0).abs() < 0.5,
            "{q:?}"
        );
        assert_eq!(
            q.quad_corners(),
            [
                (q.left, q.top),
                (q.right, q.top),
                (q.left, q.bottom),
                (q.right, q.bottom)
            ]
        );
        // A highlight must actually tint the render.
        let img = doc.render_page(0, 1.0, Theme::Light).unwrap();
        let px = |x: u32, y: u32| {
            let i = ((y * img.width + x) * 4) as usize;
            [img.data[i], img.data[i + 1], img.data[i + 2]]
        };
        let inside = px(80, 50);
        assert!(
            inside[0] > 200 && inside[1] > 200 && inside[2] < 120,
            "yellowish: {inside:?}"
        );
    }

    /// Reading highlights *after* rendering used to segfault: the render makes pdfium synthesise
    /// an appearance stream, after which pdfium-render's colour accessors take a fallback that
    /// casts the annotation handle to a page-object handle. See [`annotation_color`].
    #[test]
    fn highlights_survive_a_prior_render() {
        let Some((_d, doc)) = open_tiny_with(true) else {
            return;
        };
        doc.render_page(0, 2.0, Theme::Light).unwrap();
        let hls = doc.highlights().unwrap();
        assert_eq!(hls.len(), 1);
        assert_eq!(
            hls[0].color,
            [255, 255, 0, 255],
            "still yellow after a render"
        );
        assert_eq!(hls[0].quads.len(), 1);
        // And again, now that the appearance stream definitely exists.
        assert_eq!(doc.highlights().unwrap(), hls);
    }

    /// pdfium aborts the process if two threads call into it at once; the lock in this module is
    /// the only thing preventing that, since pdfium-render 0.9 no longer serialises calls itself.
    #[test]
    fn concurrent_use_does_not_abort() {
        let Some((dir, doc)) = open_tiny() else {
            return;
        };
        let path = dir.path().join("tiny.pdf");
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    for _ in 0..5 {
                        let other = PdfDoc::open(&path).unwrap();
                        assert_eq!(other.page_count(), 1);
                        doc.render_page(0, 1.0, Theme::Dark).unwrap();
                        assert!(!doc.page_text(0).unwrap().is_empty());
                        doc.highlights().unwrap();
                    }
                });
            }
        });
    }

    #[test]
    fn dark_theme_inverts_lightness_but_keeps_hue() {
        let white = dark_pixel([255, 255, 255, 255]);
        assert!(white[0] < 60 && white[0] > 10, "white -> {white:?}");
        assert!(
            white[0] == white[1] && white[1] == white[2],
            "stays grey: {white:?}"
        );
        assert_eq!(white[3], 255, "alpha preserved");

        let black = dark_pixel([0, 0, 0, 255]);
        assert!(black[0] > 200, "black -> {black:?}");

        let red = dark_pixel([255, 0, 0, 255]);
        assert!(
            red[0] > red[1] + 40 && red[0] > red[2] + 40,
            "stays reddish: {red:?}"
        );
        assert!(red[1] == red[2], "hue unshifted: {red:?}");

        // A yellow highlight must remain a yellow highlight.
        let yellow = dark_pixel([255, 255, 0, 255]);
        assert!(
            yellow[0] > yellow[2] + 40 && yellow[1] > yellow[2] + 40,
            "{yellow:?}"
        );
    }

    #[test]
    fn line_groups_split_on_a_new_line() {
        let g = |ch, left: f32, top: f32| Glyph {
            ch,
            rect: Rect {
                left,
                top,
                right: left + 5.0,
                bottom: top + 10.0,
            },
            index: 0,
        };
        let glyphs = [g('a', 0.0, 0.0), g('b', 5.0, 0.0), g('c', 0.0, 12.0)];
        assert_eq!(line_groups(&glyphs), vec![0..2, 2..3]);
        assert_eq!(item_offset(&line_groups(&glyphs), 2), (1, 0));
        assert_eq!(line_groups(&[]), Vec::<Range<usize>>::new());
    }
}
