//! PDF rendering, glyph geometry, selection links and highlight annotations, via pdfium.
//!
//! Everything here is plain data (no UI toolkit types) so the same code serves the GTK app,
//! the CLI and — later — Android through uniffi.
//!
//! Coordinates: all [`Rect`]s in this module are **page points with the origin at the top-left**
//! and y growing downwards, matching how a viewport draws them. pdfium's own coordinate space has
//! the origin at the bottom-left; the conversion happens at the boundary in [`Rect::from_pdf`] /
//! [`Rect::to_pdf`].

mod annot;
mod doc;
mod ink;
pub mod ledger;
mod pages;
#[cfg(test)]
mod tests;
mod text;

pub use crate::page_edit::PageEdit;
pub use annot::{LinkHighlights, highlight_quads};
pub use doc::{A4, PdfDoc, blank_pdf};
pub use ink::{Drawn, IDENTITY, Matrix, apply, catmull_rom, cut, hit, invert, swept, thin};
pub use ledger::{Ink, NamedInk, Walked, fresh_id};
pub use text::{line_top, link_with_alias, same_quads, selection_link, selection_quads};

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use anyhow::{Result, anyhow};
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

pub(super) fn pdfium() -> Result<&'static Pdfium> {
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

pub(super) fn lock() -> MutexGuard<'static, ()> {
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

    /// The rectangle two corners span, in either order: what a drag from `a` to `b` covers.
    pub fn from_corners(a: (f32, f32), b: (f32, f32)) -> Rect {
        Rect {
            left: a.0.min(b.0),
            top: a.1.min(b.1),
            right: a.0.max(b.0),
            bottom: a.1.max(b.1),
        }
    }

    /// Whether a point is inside, edges included.
    pub fn contains(&self, (x, y): (f32, f32)) -> bool {
        (self.left..=self.right).contains(&x) && (self.top..=self.bottom).contains(&y)
    }

    pub(super) fn from_pdf(r: PdfRect, page_height: f32) -> Self {
        Rect {
            left: r.left().value,
            top: page_height - r.top().value,
            right: r.right().value,
            bottom: page_height - r.bottom().value,
        }
    }

    pub(super) fn to_pdf(self, page_height: f32) -> PdfRect {
        PdfRect::new_from_values(
            page_height - self.bottom,
            self.left,
            page_height - self.top,
            self.right,
        )
    }

    /// The same rectangle with `by` points of room on every side, which is the grip a hit test
    /// allows itself.
    pub fn grow(self, by: f32) -> Rect {
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

/// How a free-hand stroke is drawn: what the pen, the highlighter and anything later put on the
/// page differ by.
///
/// `multiply` is what makes a highlighter one: the stroke darkens what is under it instead of
/// covering it, so the text stays readable through the colour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InkStyle {
    /// Stroke width in page points.
    pub width: f32,
    pub rgba: [u8; 4],
    pub multiply: bool,
}

/// One `/Ink` annotation as the eraser sees it: where it sits in the page's `/Annots`, and the
/// points of the path it draws.
pub type InkPath = (usize, Vec<(f32, f32)>);

/// A shape drawn in one drag, in top-left page points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shape {
    Line { a: (f32, f32), b: (f32, f32) },
    Rect(Rect),
    Circle { centre: (f32, f32), radius: f32 },
}

/// One `/Ink` annotation as the Adjust tool sees it: its place in `/Annots`, its path flattened
/// to a polyline, its `/Rect`, and how it is drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct InkShape {
    pub index: usize,
    pub points: Vec<(f32, f32)>,
    pub bounds: Rect,
    pub style: InkStyle,
    /// Whether what was read is all it draws, where it draws it: one path of one stroke, inside
    /// its `/Rect`. Ours always is. Another editor's may hold several paths, or draw in a space of
    /// its own that its appearance stream maps onto the page — and a cut redraws what was read,
    /// so it would lose the rest or put the pieces somewhere else. Only these are cut.
    pub cuttable: bool,
}

/// What a cut did to one stroke: what it drew, the pieces drawn in its place, and how much of the
/// page that changed.
#[derive(Debug, Clone)]
pub struct Cut {
    pub was: Drawn,
    pub left: Vec<Drawn>,
    pub area: Rect,
}

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
