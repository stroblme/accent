//! PDF rendering, reading and editing, separated from the window's commands.
//!
//! `render` owns pdfium on a worker; `protocol` carries requests and replies. `view` paints
//! cached tiles without calling pdfium; `geometry` lays them out and `cache` budgets them.
//! `tab` owns reading state, with `input`, `reply` and `disk` handling input, results and files.
//! `tools` and `ring` provide drawing geometry and options; `comment` shows annotations.
//! `organize` and `drop` edit pages, `export` writes print/export copies, `source` supplies
//! SyncTeX, and `window` integrates PDF tabs and commands with the app.
//!
//! Re-exports preserve the window's `pdfview::` and `pdftab::` names.

pub mod cache;
mod comment;
mod disk;
mod drop;
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
