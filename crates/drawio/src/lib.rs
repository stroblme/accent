//! Read, draw and edit draw.io (`.drawio`) diagrams.
//!
//! [`File`] preserves unknown XML so diagrams remain compatible with draw.io.
//! [`scene()`] produces a toolkit-independent display list; [`Editor`] edits with undo.
//! GTK and Android share this model without UI types or dependencies on other accent crates.
//!
//! Parts are ported from mxGraph and draw.io (Apache-2.0); see file headers and `NOTICE`.

mod base64;
pub mod clipboard;
pub mod edit;
pub mod geom;
pub mod guide;
pub mod handle;
mod hit;
pub mod label;
mod marker;
pub mod model;
mod perimeter;
mod placeholders;
pub mod route;
pub mod scene;
pub mod shapes;
pub mod style;
pub mod text;
mod xml;

pub use edit::{Editor, ZOrder};
pub use geom::{PathCmd, Point, Rect};
pub use label::{Marks, Run};
pub use model::{Cell, CellId, File, Page};
pub use placeholders::{Context, Now};
pub use route::{Constraint, anchors};
pub use scene::{
    Align, Cap, Font, ImageSource, Join, Paint, Prim, Scene, Stroke, VAlign, scene, scene_with,
};
pub use style::{Color, Resolved, Style, presets};
pub use xml::decode_data_uri;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Well-formed XML that is not a draw.io diagram.
    #[error("not a draw.io diagram: {0}")]
    Format(String),
    #[error("malformed XML: {0}")]
    Xml(String),
    #[error("there is no page {0}")]
    NoPage(usize),
    #[error("there is no cell {0} on this page")]
    NoCell(String),
    /// An edit the model forbids, such as deleting the last page.
    #[error("{0}")]
    Refused(&'static str),
}
