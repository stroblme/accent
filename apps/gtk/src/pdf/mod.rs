//! The PDF stack: the widget a document is painted in, the tab that owns the file, and the
//! thread that renders it.
//!
//! `geometry` lays the pages out, `cache` holds the rendered tiles, `tools` is the drawing
//! tools' pure geometry and `ring` their options on the drawing ring, `protocol` is what crosses
//! the channel to the render thread, `view` is the widget and `tab` the reader around it, and
//! `organize` moves, inserts and deletes pages from the thumbnail strip. Nothing under `view`
//! calls pdfium.
//!
//! The names below are what the rest of the window says `pdfview::` and `pdftab::` to reach.

pub mod cache;
pub mod geometry;
mod organize;
pub mod preview;
pub mod protocol;
pub mod render;
pub mod ring;
pub mod selection;
pub mod tab;
pub mod tools;
pub mod view;

pub use cache::{LOWRES_W, TILE, TileKey, Want};
pub use geometry::{Anchor, MAX_SCALE, MIN_SCALE, PdfZoom, Span, zoom_label};
pub use protocol::{Highlights, Reply};
pub use ring::Choice;
pub use tools::{Mode, shape_of};
pub use view::PdfView;
