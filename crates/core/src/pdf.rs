//! PDF rendering, glyph geometry, selection links and highlight annotations, via pdfium.
//!
//! Everything here is plain data (no UI toolkit types) so the same code serves the GTK app,
//! the CLI and — later — Android through uniffi.
//!
//! Coordinates: all [`Rect`]s in this module are **page points with the origin at the top-left**
//! and y growing downwards, matching how a viewport draws them. pdfium's own coordinate space has
//! the origin at the bottom-left; the conversion happens at the boundary in [`Rect::from_pdf`] /
//! [`Rect::to_pdf`].

use std::collections::HashMap;
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
/// `ACCENT_PDFIUM_DIR` overrides it, then `../lib/accent` next to the executable, which is where
/// `make install` and the Flatpak manifest put the library; otherwise `<workspace>/vendor/pdfium`.
///
// ponytail: the last fallback is baked in from `CARGO_MANIFEST_DIR` at build time and is only
// correct in this checkout, which is all `cargo test` and `cargo run` need. The exe-relative step
// above it is what a shipped desktop app needs. Android is still uncovered: the APK's `jniLibs`
// has no such layout, so there this yields a nonexistent directory and `bind_to_system_library()`
// does the work.
pub fn library_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("ACCENT_PDFIUM_DIR") {
        return PathBuf::from(dir);
    }
    // The probe spells the file name out while the binding below derives it from the platform.
    // They agree on every target that reaches this branch (Linux, and the exe-relative layout is
    // a desktop install), and keeping the probe a plain `exists()` keeps it readable.
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|bin| bin.join("../lib/accent")))
        .filter(|dir| dir.join("libpdfium.so").exists())
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/pdfium"))
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

    /// The same rectangle with `by` points of room on every side.
    fn grow(self, by: f32) -> Rect {
        Rect {
            left: self.left - by,
            top: self.top - by,
            right: self.right + by,
            bottom: self.bottom + by,
        }
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

/// One `/Ink` annotation as the eraser sees it: where it sits in the page's `/Annots`, and the
/// points of the path it draws.
pub type InkPath = (usize, Vec<(f32, f32)>);

/// A rendered page. `data` is tightly packed RGBA8, `width * height * 4` bytes.
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// How a page is recoloured on its way to the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Theme {
    /// Exactly as the document defines it.
    Plain,
    /// Remapped so the document's paper lands on `paper` and its ink on `ink`, keeping each
    /// pixel's own chroma so a coloured figure stays coloured.
    Recolour { paper: [u8; 3], ink: [u8; 3] },
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

/// Where a `/Link` annotation points.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LinkTarget {
    /// Another page of the same document. `top` is the y the viewer should scroll to, in points
    /// of the *target* page, top-left origin; `None` means "keep the current position".
    Page {
        page: usize,
        top: Option<f32>,
    },
    Uri(String),
}

/// A `/Link` annotation: the clickable box and where it leads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Link {
    pub rect: Rect,
    pub target: LinkTarget,
}

/// One entry of the document outline, flattened depth-first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outline {
    pub depth: usize,
    pub title: String,
    pub page: Option<usize>,
}

// ---------------------------------------------------------------------------------------------
// Recolouring
// ---------------------------------------------------------------------------------------------

/// Move a pixel onto the theme's paper–ink ramp while keeping its chroma, so white paper becomes
/// `paper` and black text becomes `ink`, but a yellow highlight stays yellow.
///
/// The pixel's luminance picks a point on the ramp; the pixel's own offset from its luminance is
/// then added back per channel, which is what carries the colour across.
///
// ponytail: this keeps the *absolute* chroma offset `c - luminance` and clamps, which is one
// multiply-free pass over the buffer and good enough to read by. The ceiling: saturated colours
// near the ends of the ramp lose some saturation to the clamp, and it is not a real perceptual
// space. Upgrade path when someone complains is Oklab — convert, remap L, convert back — at
// roughly 3x the cost, at which point this wants SIMD or the GPU.
pub fn recolour_pixel(px: [u8; 4], paper: [u8; 3], ink: [u8; 3]) -> [u8; 4] {
    let (r, g, b) = (
        px[0] as f32 / 255.0,
        px[1] as f32 / 255.0,
        px[2] as f32 / 255.0,
    );
    let l = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let map = |i: usize, c: f32| {
        let (ink, paper) = (ink[i] as f32 / 255.0, paper[i] as f32 / 255.0);
        let target = ink + l * (paper - ink);
        ((target + (c - l)) * 255.0).clamp(0.0, 255.0) as u8
    };
    [map(0, r), map(1, g), map(2, b), px[3]]
}

/// [`recolour_pixel`] over a whole RGBA8 buffer, in place. Shared by the page and tile renders so
/// the two cannot drift apart.
fn recolour(data: &mut [u8], paper: [u8; 3], ink: [u8; 3]) {
    for px in data.as_chunks_mut::<4>().0 {
        let out = recolour_pixel([px[0], px[1], px[2], px[3]], paper, ink);
        px.copy_from_slice(&out);
    }
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

    /// Existing `/Highlight` annotations on one page.
    pub fn highlights_on(&self, page: usize) -> Result<Vec<Highlight>> {
        let _guard = lock();
        Ok(highlights_of(page, &self.page(page)?))
    }

    /// Existing `/Highlight` annotations across the whole document.
    pub fn highlights(&self) -> Result<Vec<Highlight>> {
        let _guard = lock();
        Ok(self
            .doc()
            .pages()
            .iter()
            .enumerate()
            .flat_map(|(page, p)| highlights_of(page, &p))
            .collect())
    }

    /// Write `/Highlight` annotations into the in-memory document, and say how many were written.
    ///
    /// A highlight whose quads a highlight on that page already covers is skipped, so exporting
    /// the same note links twice adds nothing and the painted overlay can step aside for the
    /// annotation as soon as it is in the file.
    ///
    /// Nothing reaches the disk here: [`PdfDoc::save`] is the second half.
    pub fn add_highlights(&mut self, highlights: &[Highlight]) -> Result<usize> {
        let _guard = lock();
        let mut added = 0;
        for h in highlights {
            let mut p = self.page(h.page)?;
            // Manual: under the default every annotation added re-serialises the *page's* content
            // stream, which we never touch. The annotation's own appearance stream is written by
            // pdfium either way.
            p.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
            if highlights_of(h.page, &p)
                .iter()
                .any(|e| same_quads(&e.quads, &h.quads))
            {
                continue;
            }
            let height = p.height().value;
            let bounds = h
                .quads
                .iter()
                .copied()
                .reduce(Rect::union)
                .unwrap_or(Rect::ZERO);
            let mut a = p
                .annotations_mut()
                .create_highlight_annotation()
                .context("create highlight annotation")?;
            // The colour first, and never `fill_color`/`stroke_color` as *getters*: both setters
            // fall back to casting the annotation handle to a page-object handle once an
            // appearance stream exists, which is the crash `annotation_color` documents. On a
            // fresh annotation the safe branch is the one that runs.
            a.set_stroke_color(PdfColor::new(
                h.color[0], h.color[1], h.color[2], h.color[3],
            ))
            .context("highlight colour")?;
            // Before the quads: pdfium copies `/Rect` into the appearance stream's bounding box.
            a.set_bounds(bounds.to_pdf(height))
                .context("highlight bounds")?;
            for q in &h.quads {
                let [tl, tr, bl, br] = q.quad_corners();
                let y = |v: f32| height - v;
                a.attachment_points_mut()
                    .create_attachment_point_at_end(PdfQuadPoints::new_from_values(
                        tl.0,
                        y(tl.1),
                        tr.0,
                        y(tr.1),
                        bl.0,
                        y(bl.1),
                        br.0,
                        y(br.1),
                    ))
                    .context("highlight quad")?;
            }
            if let Some(text) = &h.contents {
                a.set_contents(text).context("highlight contents")?;
            }
            a.set_is_printed(true).context("highlight print flag")?;
            added += 1;
        }
        Ok(added)
    }

    /// Draw one free-hand stroke onto a page as an `/Ink` annotation.
    ///
    /// `points` are in this module's top-left-origin page points, `width` is the stroke width in
    /// points, `rgb` its colour.
    ///
    // ponytail: the geometry ends up in the annotation's *appearance stream* rather than in an
    // `/InkList` array, because `FPDFAnnot_AddInkStroke` is only reachable through the raw
    // bindings and pdfium-render keeps the annotation handle private. Every viewer renders the
    // appearance stream, so this draws correctly everywhere; what it costs is an editor that
    // wants to reshape the stroke, which would need `/InkList`. Raw bindings are the upgrade.
    pub fn add_ink(
        &mut self,
        page: usize,
        points: &[(f32, f32)],
        width: f32,
        rgb: [u8; 3],
    ) -> Result<()> {
        let thinned = thin(points, 1.5);
        let Some(&first) = thinned.first() else {
            return Ok(());
        };
        let _guard = lock();
        let mut p = self.page(page)?;
        p.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
        let height = p.height().value;
        let y = |v: f32| PdfPoints::new(height - v);
        let colour = PdfColor::new(rgb[0], rgb[1], rgb[2], 255);

        let bounds = thinned
            .iter()
            .fold(
                Rect {
                    left: first.0,
                    top: first.1,
                    right: first.0,
                    bottom: first.1,
                },
                |r, &(x, y)| {
                    r.union(Rect {
                        left: x,
                        top: y,
                        right: x,
                        bottom: y,
                    })
                },
            )
            .grow(width / 2.0 + 1.0);

        let mut path = PdfPagePathObject::new(
            self.doc(),
            PdfPoints::new(first.0),
            y(first.1),
            Some(colour),
            Some(PdfPoints::new(width)),
            None,
        )
        .context("ink path")?;
        path.set_line_cap(PdfPageObjectLineCap::Round)
            .context("ink line cap")?;
        path.set_line_join(PdfPageObjectLineJoin::Round)
            .context("ink line join")?;
        match thinned.len() {
            // A stroke that never moved: a zero-length segment, which the round cap draws as the
            // dot the reader meant.
            1 => path
                .line_to(PdfPoints::new(first.0), y(first.1))
                .context("ink dot")?,
            _ => {
                for [c1, c2, end] in catmull_rom(&thinned) {
                    path.bezier_to(
                        PdfPoints::new(end.0),
                        y(end.1),
                        PdfPoints::new(c1.0),
                        y(c1.1),
                        PdfPoints::new(c2.0),
                        y(c2.1),
                    )
                    .context("ink segment")?;
                }
            }
        }

        let mut ink = p
            .annotations_mut()
            .create_ink_annotation()
            .context("create ink annotation")?;
        ink.set_stroke_color(colour).context("ink colour")?;
        // Before the object: pdfium copies `/Rect` into the appearance stream's bounding box, and
        // a box set afterwards would scale what was drawn into it.
        ink.set_bounds(bounds.to_pdf(height))
            .context("ink bounds")?;
        ink.objects_mut()
            .add_object(path.into())
            .context("ink object")?;
        ink.set_is_printed(true).context("ink print flag")?;
        Ok(())
    }

    /// How many annotations of every kind a page carries.
    pub fn annotation_count(&self, page: usize) -> Result<usize> {
        let _guard = lock();
        Ok(self.page(page)?.annotations().len())
    }

    /// Remove one annotation by its index in the page's `/Annots`.
    pub fn delete_annotation(&mut self, page: usize, index: usize) -> Result<()> {
        let _guard = lock();
        let mut p = self.page(page)?;
        p.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
        // Through `annotations_mut` rather than `annotations`: the annotation has to carry the
        // document's lifetime for `delete_annotation` to take it, and the shared accessor hands
        // back one borrowed from `p` instead.
        let a = p
            .annotations_mut()
            .get(index as PdfPageAnnotationIndex)
            .map_err(|e| anyhow!("annotation {index} of page {page}: {e:?}"))?;
        p.annotations_mut()
            .delete_annotation(a)
            .context("delete annotation")?;
        Ok(())
    }

    /// Every `/Ink` annotation on a page with the points of its drawn path, for the eraser to
    /// aim at. The index is the annotation's place in `/Annots`, which is what deletes it.
    ///
    // ponytail: the points are read straight out of the appearance path, so a stroke drawn by
    // another editor whose appearance stream carries a `/Matrix` is hit-tested in form space and
    // may not answer to the pointer. Ours never do; a transform-aware read is the upgrade.
    pub fn ink_paths(&self, page: usize) -> Result<Vec<InkPath>> {
        let _guard = lock();
        let p = self.page(page)?;
        let height = p.height().value;
        Ok(p.annotations()
            .iter()
            .enumerate()
            .filter(|(_, a)| a.annotation_type() == PdfPageAnnotationType::Ink)
            .map(|(i, a)| {
                let points = a
                    .objects()
                    .iter()
                    .filter_map(|o| {
                        Some(
                            o.as_path_object()?
                                .segments()
                                .iter()
                                .map(|s| (s.x().value, height - s.y().value))
                                .collect::<Vec<_>>(),
                        )
                    })
                    .flatten()
                    .collect();
                (i, points)
            })
            .collect())
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
/// The `/Highlight` annotations of one loaded page. Shared by the whole-document read, the
/// per-page one and the export's own "is this already here" check, so the three cannot drift.
fn highlights_of(page: usize, p: &PdfPage<'_>) -> Vec<Highlight> {
    let page_height = p.height().value;
    let mut out = Vec::new();
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
    out
}

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
// Selections
// ---------------------------------------------------------------------------------------------

/// One rectangle per visual line the glyph range `start..end` touches.
fn quads_between(glyphs: &[Glyph], lines: &[Range<usize>], start: usize, end: usize) -> Vec<Rect> {
    lines
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
        .collect()
}

/// Build an Obsidian-compatible link for a glyph range, plus the quads and text we keep for
/// re-anchoring.
///
/// Takes the page's glyphs rather than reading them: the caller that has a selection on screen
/// already holds them, and [`selection_quads`] has to walk the same lines to paint it again.
///
// ponytail: Obsidian's `selection=a,b,c,d` are PDF.js text-*item* indices with a character
// offset inside each item, and PDF.js segments a page differently from pdfium — there is no
// way to reproduce its numbering from here, so cross-engine fidelity is best-effort: a link we
// emit may land a few characters off when opened in Obsidian, and vice versa. That is why
// `SelectionLink` also carries `quads` and `text`: our own viewer re-anchors from those
// (quads first, text search as the fallback) and treats the numbers as a hint only.
pub fn selection_link(glyphs: &[Glyph], rel_pdf_path: &str, sel: &Selection) -> SelectionLink {
    let start = sel.start.min(glyphs.len());
    let end = sel.end.clamp(start, glyphs.len());
    let lines = line_groups(glyphs);

    let (a, b) = item_offset(&lines, start);
    let (c, d) = item_offset(&lines, end.saturating_sub(1));

    SelectionLink {
        // `d + 1`: PDF.js's end offset is exclusive.
        link: format!(
            "[[{rel_pdf_path}#page={}&selection={a},{b},{c},{}]]",
            sel.page + 1,
            d + 1
        ),
        quads: quads_between(glyphs, &lines, start, end),
        text: glyphs[start..end].iter().map(|g| g.ch).collect(),
    }
}

/// The reverse of [`selection_link`]: what `a,b,c,d` covers on this page today.
///
/// `None` when the numbers do not fit the page's lines, which is what a link written against
/// another engine's numbering, or against an edition of the document with different line breaks,
/// looks like. The caller falls back to searching the text the link quotes.
pub fn selection_quads(glyphs: &[Glyph], sel: [usize; 4]) -> Option<(Range<usize>, Vec<Rect>)> {
    let [a, b, c, d] = sel;
    let lines = line_groups(glyphs);
    let (first, last) = (lines.get(a)?, lines.get(c)?);
    let start = first.start + b;
    let end = last.start + d;
    if start > first.end || end > last.end || end <= start {
        return None;
    }
    Some((start..end, quads_between(glyphs, &lines, start, end)))
}

/// Whether two quad lists cover the same place, to half a point.
///
/// What tells an exported highlight from the note link it came from, so a second export writes
/// nothing and the painted overlay steps aside once the annotation is in the file.
pub fn same_quads(a: &[Rect], b: &[Rect]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            let close = |p: f32, q: f32| (p - q).abs() < 0.5;
            close(x.left, y.left)
                && close(x.top, y.top)
                && close(x.right, y.right)
                && close(x.bottom, y.bottom)
        })
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

// ---------------------------------------------------------------------------------------------
// Ink geometry
// ---------------------------------------------------------------------------------------------
//
// Pure, and here rather than in the widget so Android draws the same curve through the same
// three functions.

/// Drop points closer than `min` to the last one kept.
///
/// A pointer reports on every motion event, so a slow stroke arrives as a cloud of near-identical
/// points; thinning first is what keeps the curve below from wobbling between them.
pub fn thin(points: &[(f32, f32)], min: f32) -> Vec<(f32, f32)> {
    let mut out: Vec<(f32, f32)> = Vec::new();
    for &p in points {
        let far = out
            .last()
            .is_none_or(|&(x, y)| (p.0 - x).hypot(p.1 - y) >= min);
        if far {
            out.push(p);
        }
    }
    // A stroke that never moved is still a dot, not nothing.
    if out.is_empty()
        && let Some(&p) = points.first()
    {
        out.push(p);
    }
    out
}

/// A Catmull-Rom spline through `points` as cubic Béziers: `[control 1, control 2, end]` per
/// segment, ready for `bezier_to`.
///
// ponytail: the ends repeat the first and last point rather than extrapolating a phantom one,
// which is the standard clamped form and means a stroke starts and ends exactly where the pointer
// did. Not Savitzky-Golay (what UNote uses): that wants a least-squares solve over a 31-sample
// window, lags the pointer by half of it, and has to be re-run over the whole stroke on every
// motion event. Catmull-Rom is local, closed-form, and its output is the argument `bezier_to`
// already takes.
pub fn catmull_rom(points: &[(f32, f32)]) -> Vec<[(f32, f32); 3]> {
    let at = |i: isize| points[(i.max(0) as usize).min(points.len() - 1)];
    (0..points.len().saturating_sub(1))
        .map(|i| {
            let i = i as isize;
            let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
            [
                (p1.0 + (p2.0 - p0.0) / 6.0, p1.1 + (p2.1 - p0.1) / 6.0),
                (p2.0 - (p3.0 - p1.0) / 6.0, p2.1 - (p3.1 - p1.1) / 6.0),
                p2,
            ]
        })
        .collect()
}

/// Whether `at` lies within `radius` of the polyline through `points` — the eraser's hit test.
pub fn hit(points: &[(f32, f32)], at: (f32, f32), radius: f32) -> bool {
    let near = |a: (f32, f32), b: (f32, f32)| {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = dx * dx + dy * dy;
        // A degenerate segment is its own endpoint.
        let t = match len2 > f32::EPSILON {
            true => (((at.0 - a.0) * dx + (at.1 - a.1) * dy) / len2).clamp(0.0, 1.0),
            false => 0.0,
        };
        (at.0 - (a.0 + t * dx)).hypot(at.1 - (a.1 + t * dy)) <= radius
    };
    match points {
        [] => false,
        [only] => near(*only, *only),
        _ => points.windows(2).any(|w| near(w[0], w[1])),
    }
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A two-page PDF built by hand so the tests need no fixture: a line of Helvetica on each
    /// page, an internal link from page 1 to page 2, a URI link on page 2, and a two-level
    /// outline. With `highlight`, page 1 also carries a `/Highlight` annotation over its line —
    /// listed *before* the link in `/Annots`, which is what trips a links-by-annotation-index
    /// implementation.
    fn tiny_pdf(highlight: bool) -> Vec<u8> {
        let content1 = "BT /F1 24 Tf 20 40 Td (Hello accent) Tj ET";
        let content2 = "BT /F1 18 Tf 20 40 Td (Second page) Tj ET";
        // Object 6 is written either way so the numbering below never shifts; without
        // `highlight` nothing references it and pdfium never sees it.
        let annots1 = if highlight {
            "/Annots[6 0 R 7 0 R]"
        } else {
            "/Annots[7 0 R]"
        };
        let objs = vec![
            "<</Type/Catalog/Pages 2 0 R/Outlines 8 0 R>>".to_string(),
            "<</Type/Pages/Kids[3 0 R 9 0 R]/Count 2>>".to_string(),
            format!(
                "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]\
                 /Resources<</Font<</F1 4 0 R>>>>/Contents 5 0 R{annots1}>>"
            ),
            "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".to_string(),
            format!(
                "<</Length {}>>stream\n{content1}\nendstream",
                content1.len()
            ),
            // /QuadPoints is in the PDF spec's order: upper-left, upper-right, lower-left,
            // lower-right, bottom-left page origin.
            "<</Type/Annot/Subtype/Highlight/Rect[18 36 140 64]\
             /QuadPoints[18 64 140 64 18 36 140 36]/C[1 1 0]/CA 1\
             /Contents(check this)/F 4>>"
                .to_string(),
            "<</Type/Annot/Subtype/Link/Rect[20 30 140 60]/Border[0 0 0]\
             /Dest[9 0 R /XYZ 0 80 0]>>"
                .to_string(),
            "<</Type/Outlines/First 12 0 R/Last 12 0 R/Count 2>>".to_string(),
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]\
             /Resources<</Font<</F1 4 0 R>>>>/Contents 10 0 R/Annots[11 0 R]>>"
                .to_string(),
            format!(
                "<</Length {}>>stream\n{content2}\nendstream",
                content2.len()
            ),
            "<</Type/Annot/Subtype/Link/Rect[10 10 100 30]/Border[0 0 0]\
             /A<</S/URI/URI(https://example.org)>>>>"
                .to_string(),
            "<</Title(Second)/Parent 8 0 R/First 13 0 R/Last 13 0 R/Count 1\
             /Dest[9 0 R /XYZ 0 80 0]>>"
                .to_string(),
            "<</Title(Child)/Parent 12 0 R/Dest[9 0 R /XYZ 0 60 0]>>".to_string(),
        ];
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
        assert_eq!(doc.page_count(), 2);
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
        let glyphs = doc.page_text(0).unwrap();
        let out = selection_link(&glyphs, "notes/paper.pdf", &sel);
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

    /// Save the document into its own temporary directory and open the copy.
    fn reopen(dir: &tempfile::TempDir, doc: &PdfDoc) -> PdfDoc {
        let out = dir.path().join("out.pdf");
        std::fs::write(&out, doc.save().unwrap()).unwrap();
        PdfDoc::open(&out).unwrap()
    }

    #[test]
    fn exported_highlights_read_back_after_a_reopen() {
        let Some((dir, mut doc)) = open_tiny() else {
            return;
        };
        let hl = Highlight {
            page: 0,
            quads: vec![Rect {
                left: 18.0,
                top: 36.0,
                right: 140.0,
                bottom: 64.0,
            }],
            color: [255, 255, 0, 255],
            contents: Some("check this".to_string()),
        };
        assert_eq!(doc.add_highlights(std::slice::from_ref(&hl)).unwrap(), 1);
        // The same quads again are the export the reader asked for twice.
        assert_eq!(doc.add_highlights(std::slice::from_ref(&hl)).unwrap(), 0);

        let back = reopen(&dir, &doc);
        let hls = back.highlights().unwrap();
        assert_eq!(hls.len(), 1, "{hls:?}");
        assert_eq!(hls[0].page, 0);
        assert_eq!(hls[0].contents.as_deref(), Some("check this"));
        assert_eq!(hls[0].color, [255, 255, 0, 255]);
        assert!(same_quads(&hls[0].quads, &hl.quads), "{:?}", hls[0].quads);
        // And it tints the page, the way the fixture's own highlight does.
        let img = back.render_page(0, 1.0, Theme::Plain).unwrap();
        let i = ((50 * img.width + 80) * 4) as usize;
        let px = [img.data[i], img.data[i + 1], img.data[i + 2]];
        assert!(
            px[0] > 200 && px[1] > 200 && px[2] < 120,
            "yellowish: {px:?}"
        );

        // A page-level read sees the same one, and the other page has none.
        assert_eq!(back.highlights_on(0).unwrap().len(), 1);
        assert!(back.highlights_on(1).unwrap().is_empty());
    }

    #[test]
    fn ink_round_trips_through_save() {
        let Some((dir, mut doc)) = open_tiny() else {
            return;
        };
        let before = doc.annotation_count(0).unwrap();
        doc.add_ink(
            0,
            &[(20.0, 20.0), (60.0, 40.0), (100.0, 20.0)],
            4.0,
            [255, 0, 0],
        )
        .unwrap();
        assert_eq!(doc.annotation_count(0).unwrap(), before + 1);

        let strokes = doc.ink_paths(0).unwrap();
        assert_eq!(strokes.len(), 1, "{strokes:?}");
        let start = strokes[0].1[0];
        assert!(
            (start.0 - 20.0).abs() < 0.5 && (start.1 - 20.0).abs() < 0.5,
            "{start:?}"
        );

        // It survives the file, which is the whole point of drawing into the PDF.
        let back = reopen(&dir, &doc);
        assert_eq!(back.annotation_count(0).unwrap(), before + 1);
        assert_eq!(back.ink_paths(0).unwrap().len(), 1);
        // And it is drawn: the stroke passes through the middle of the page in red.
        let img = back.render_page(0, 1.0, Theme::Plain).unwrap();
        let red = img
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .any(|px| px[0] > 200 && px[1] < 100 && px[2] < 100);
        assert!(red, "the stroke is drawn");

        // Erasing takes the whole stroke and leaves what was there before.
        let mut back = back;
        back.delete_annotation(0, strokes[0].0).unwrap();
        assert_eq!(back.annotation_count(0).unwrap(), before);
        assert!(back.ink_paths(0).unwrap().is_empty());
    }

    #[test]
    fn blank_pdf_is_one_a4_page() {
        if !available() {
            eprintln!("skipping: no libpdfium");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sketch.pdf");
        std::fs::write(&path, blank_pdf().unwrap()).unwrap();
        let doc = PdfDoc::open(&path).unwrap();
        assert_eq!(doc.page_count(), 1);
        let (w, h) = doc.page_size(0).unwrap();
        assert!(
            (w - 595.3).abs() < 1.0 && (h - 841.9).abs() < 1.0,
            "{w}x{h}"
        );
    }

    #[test]
    fn thin_catmull_rom_and_hit() {
        // Thinning keeps the ends and drops what sits inside the step.
        let pts = [(0.0, 0.0), (0.5, 0.0), (3.0, 0.0)];
        assert_eq!(thin(&pts, 1.0), vec![(0.0, 0.0), (3.0, 0.0)]);
        // A stroke that never moved is a dot, not an empty path.
        assert_eq!(thin(&[(1.0, 2.0), (1.0, 2.0)], 1.0), vec![(1.0, 2.0)]);
        assert!(thin(&[], 1.0).is_empty());

        // Three points on a line give two segments that stay on it and end where they should.
        let segs = catmull_rom(&[(0.0, 0.0), (10.0, 0.0), (20.0, 0.0)]);
        assert_eq!(segs.len(), 2);
        assert!(segs.iter().flatten().all(|p| p.1 == 0.0), "{segs:?}");
        assert_eq!(segs[0][2], (10.0, 0.0));
        assert_eq!(segs[1][2], (20.0, 0.0));
        assert!(catmull_rom(&[(1.0, 1.0)]).is_empty());

        let line = [(0.0, 0.0), (10.0, 0.0)];
        assert!(hit(&line, (5.0, 3.0), 4.0));
        assert!(!hit(&line, (5.0, 5.0), 4.0));
        // Past the end of the segment, not just off its side.
        assert!(!hit(&line, (20.0, 0.0), 4.0));
        assert!(hit(&[(0.0, 0.0)], (2.0, 0.0), 4.0));
    }

    #[test]
    fn selection_quads_round_trips_selection_link() {
        let Some((_d, doc)) = open_tiny() else { return };
        let glyphs = doc.page_text(0).unwrap();
        let sel = Selection {
            page: 0,
            start: 0,
            end: 12,
        };
        let out = selection_link(&glyphs, "paper.pdf", &sel);
        let anchor = out.link.split_once('#').unwrap().1.trim_end_matches("]]");
        let (page, nums) = crate::markdown::pdf_anchor(anchor).unwrap();
        assert_eq!(page, 0);

        let (range, quads) = selection_quads(&glyphs, nums.unwrap()).unwrap();
        assert_eq!(range, 0..12);
        assert!(same_quads(&quads, &out.quads), "{quads:?} {:?}", out.quads);

        // Numbers past the page's lines are a link from another engine, not a panic.
        assert!(selection_quads(&glyphs, [99, 0, 99, 3]).is_none());
        assert!(selection_quads(&glyphs, [0, 0, 0, 0]).is_none());
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
        let img = doc.render_page(0, 1.0, Theme::Plain).unwrap();
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
        doc.render_page(0, 2.0, Theme::Plain).unwrap();
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
                        assert_eq!(other.page_count(), 2);
                        doc.render_page(0, 1.0, adwaita_dark()).unwrap();
                        assert!(!doc.page_text(0).unwrap().is_empty());
                        doc.highlights().unwrap();
                    }
                });
            }
        });
    }

    #[test]
    fn render_tile_matches_the_full_render() {
        let Some((_d, doc)) = open_tiny_with(true) else {
            return;
        };
        // 200x100pt at 2 px/pt is a 400x200 px page.
        let full = doc.render_page(0, 2.0, Theme::Plain).unwrap();
        let (x, y, w, h) = (60u32, 40u32, 120u32, 80u32);
        let tile = doc
            .render_tile(0, 2.0, x as i32, y as i32, w as i32, h as i32, Theme::Plain)
            .unwrap();
        assert_eq!((tile.width, tile.height), (w, h));
        // Byte-identical, not approximate: the tile is the same render translated by a whole
        // number of pixels, so pdfium rasterises it onto the same device grid.
        for row in 0..h {
            let src = (((y + row) * full.width + x) * 4) as usize;
            let dst = ((row * w) * 4) as usize;
            let n = (w * 4) as usize;
            assert_eq!(
                &full.data[src..src + n],
                &tile.data[dst..dst + n],
                "row {row}"
            );
        }
        // A tile running off the page keeps only what is left of it; one starting off it fails.
        let clamped = doc
            .render_tile(0, 2.0, 380, 190, 100, 100, Theme::Plain)
            .unwrap();
        assert_eq!((clamped.width, clamped.height), (20, 10));
        assert!(
            doc.render_tile(0, 2.0, 400, 0, 10, 10, Theme::Plain)
                .is_err()
        );
    }

    #[test]
    fn links_read_dest_and_uri() {
        let Some((_d, doc)) = open_tiny_with(true) else {
            return;
        };
        // Page 1 lists the highlight before the link, which is what used to yield the link twice.
        let first = doc.links(0).unwrap();
        assert_eq!(first.len(), 1, "{first:?}");
        // /Rect[20 30 140 60] on a 100pt page: top = 100 - 60, bottom = 100 - 30.
        let r = first[0].rect;
        assert!(
            (r.left - 20.0).abs() < 0.5
                && (r.top - 40.0).abs() < 0.5
                && (r.bottom - 70.0).abs() < 0.5,
            "{r:?}"
        );
        // /Dest [page 2 /XYZ 0 80 0] on a 100pt page: top-left y = 100 - 80 = 20.
        let LinkTarget::Page { page, top } = first[0].target else {
            panic!("{:?}", first[0].target);
        };
        assert_eq!(page, 1);
        assert!(top.is_some_and(|t| (t - 20.0).abs() < 0.5), "{top:?}");

        let second = doc.links(1).unwrap();
        assert_eq!(second.len(), 1, "{second:?}");
        assert_eq!(
            second[0].target,
            LinkTarget::Uri("https://example.org".to_string())
        );
    }

    #[test]
    fn outline_is_flat_with_depths() {
        let Some((_d, doc)) = open_tiny() else { return };
        let got: Vec<_> = doc
            .outline()
            .unwrap()
            .into_iter()
            .map(|o| (o.title, o.depth, o.page))
            .collect();
        assert_eq!(
            got,
            vec![
                ("Second".to_string(), 0, Some(1)),
                ("Child".to_string(), 1, Some(1)),
            ]
        );
    }

    #[test]
    fn search_is_case_insensitive_and_empty_safe() {
        let Some((_d, doc)) = open_tiny() else { return };
        let hits = doc.search(1, "second").unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert!(
            hits[0].iter().any(|r| r.width() > 0.0 && r.height() > 0.0),
            "{:?}",
            hits[0]
        );
        assert!(doc.search(1, "").unwrap().is_empty(), "empty query");
        assert!(doc.search(0, "second").unwrap().is_empty(), "other page");
    }

    // The two pairs the app asks for: the Adwaita dark view background with its foreground, and
    // Solarized light's cream paper with its slate ink.
    const ADWAITA: ([u8; 3], [u8; 3]) = ([0x1d, 0x1d, 0x20], [0xeb, 0xeb, 0xeb]);
    const SOLARIZED_LIGHT: ([u8; 3], [u8; 3]) = ([0xfd, 0xf6, 0xe3], [0x65, 0x7b, 0x83]);

    fn adwaita_dark() -> Theme {
        Theme::Recolour {
            paper: ADWAITA.0,
            ink: ADWAITA.1,
        }
    }

    #[test]
    fn recolour_maps_paper_and_ink_to_the_theme() {
        for (paper, ink) in [ADWAITA, SOLARIZED_LIGHT] {
            // Tolerance of 1 per channel: the ramp runs through f32 and truncates on the way out.
            let near = |got: [u8; 4], want: [u8; 3]| (0..3).all(|i| got[i].abs_diff(want[i]) <= 1);

            let white = recolour_pixel([255, 255, 255, 255], paper, ink);
            assert!(near(white, paper), "white -> {white:?}, want {paper:?}");
            assert_eq!(white[3], 255, "alpha preserved");

            let black = recolour_pixel([0, 0, 0, 255], paper, ink);
            assert!(near(black, ink), "black -> {black:?}, want {ink:?}");
        }
    }

    #[test]
    fn recolour_keeps_a_saturated_colour_recognisable() {
        for (paper, ink) in [ADWAITA, SOLARIZED_LIGHT] {
            // A yellow highlight must remain a yellow highlight, not turn grey.
            let yellow = recolour_pixel([255, 255, 0, 255], paper, ink);
            assert!(
                yellow[0] > yellow[2] + 40 && yellow[1] > yellow[2] + 40,
                "{yellow:?} on paper {paper:?}"
            );

            let red = recolour_pixel([255, 0, 0, 255], paper, ink);
            assert!(
                red[0] > red[1] + 40 && red[0] > red[2] + 40,
                "stays reddish: {red:?} on paper {paper:?}"
            );
        }
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
