//! The PDF stack: the widget a document is painted in, the tab that owns the file, and the
//! thread that renders it.
//!
//! `geometry` lays the pages out, `cache` holds the rendered tiles, `tools` is the drawing
//! tools' pure geometry and `ring` their options on the drawing ring, `protocol` is what crosses
//! the channel to the render thread, `view` is the widget and `tab` the reader around it, with
//! `input` wiring its views, menu and keys to it, `reply` taking the render thread's answers,
//! `disk` its file's saves and reloads and `comment` showing what other readers wrote on a page.
//! `organize` moves, inserts and deletes pages from the thumbnail strip, `export` writes the copy
//! Export as PDF and Print hand on, and `source` is the page's half of SyncTeX. `window` is the
//! window's side of a PDF tab: opening one and the PDF commands. Nothing under `view` calls
//! pdfium.
//!
//! The names below are what the rest of the window says `pdfview::` and `pdftab::` to reach.

pub mod cache;
mod comment;
mod disk;
pub mod export;
pub mod geometry;
mod input;
mod organize;
pub mod preview;
pub mod protocol;
pub mod render;
mod reply;
pub mod ring;
pub mod selection;
mod source;
pub mod tab;
pub mod tools;
pub mod view;
mod window;

pub use cache::{LOWRES_W, TILE, TileKey, Want};
pub use geometry::{Anchor, PdfZoom, Span, zoom_label};
pub use protocol::{Highlights, Reply};
pub use ring::Choice;
pub use tools::{Mode, shape_of};
pub use view::PdfView;
