//! accent-drawio: read, draw and edit draw.io (`.drawio`) diagrams.
//!
//! The file is mxGraph's XML. [`File`] keeps every part of it, understood or not, so a diagram
//! saved here opens in draw.io as it was; [`scene()`] turns a page into a display list any toolkit
//! can paint; [`Editor`] changes the model with undo. There are no UI types here and no
//! dependency on the rest of accent: the GTK app and the Android app paint the same list.
//!
//! Parts are ported from mxGraph and draw.io (Apache-2.0). Each such file says so in its header;
//! see `NOTICE`.

pub mod base64;
pub mod edit;
pub mod geom;
pub mod hit;
pub mod label;
pub mod marker;
pub mod model;
pub mod perimeter;
pub mod route;
pub mod scene;
pub mod shapes;
pub mod style;
pub mod xml;

pub use edit::{Editor, ZOrder};
pub use geom::{PathCmd, Point, Rect};
pub use label::{Marks, Run};
pub use model::{Cell, CellId, File, Geometry, Page, Value};
pub use scene::{Align, Font, ImageSource, Paint, Prim, Scene, Stroke, VAlign, scene};
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
