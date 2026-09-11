//! A page as a display list: what to paint, in paint order, in page coordinates. The toolkit
//! turns each [`Prim`] into its own drawing calls and measures text itself.

use crate::geom::{self, PathCmd, Point, Rect};
use crate::label::Run;
use crate::model::{CellId, Page};
use crate::style::Color;

/// A page ready to paint.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Scene {
    pub prims: Vec<Prim>,
    pub page_size: (f64, f64),
    /// The page colour; `None` is draw.io's default white.
    pub background: Option<Color>,
}

/// How an outline is filled.
#[derive(Debug, Clone, PartialEq)]
pub enum Paint {
    Solid(Color),
    /// A two-stop gradient running from `start` to `end`.
    Linear {
        from: Color,
        to: Color,
        start: Point,
        end: Point,
    },
}

/// How an outline is stroked. Joins are mitred and caps butt, as in draw.io's SVG.
#[derive(Debug, Clone, PartialEq)]
pub struct Stroke {
    pub color: Color,
    pub width: f64,
    /// Alternating on/off lengths in page units, already scaled by the width.
    pub dash: Option<Vec<f64>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    Left,
    #[default]
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VAlign {
    Top,
    #[default]
    Middle,
    Bottom,
}

/// A label's font where its runs do not say otherwise.
#[derive(Debug, Clone, PartialEq)]
pub struct Font {
    /// In page units (CSS pixels at zoom 1).
    pub size: f64,
    pub family: String,
    pub color: Color,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImageSource {
    /// `data:` URI as written in the style; see [`crate::decode_data_uri`].
    DataUri(String),
    /// A picture on the web, which accent does not fetch.
    Url(String),
}

/// draw.io's shadow: the shape again, offset and translucent black (`Graph.js` overrides
/// mxGraph's grey to black at a quarter).
pub const SHADOW_OFFSET: Point = Point::new(2.0, 3.0);
pub const SHADOW_COLOR: Color = Color {
    r: 0,
    g: 0,
    b: 0,
    a: 64,
};

/// One thing to paint. `cell` names the cell it belongs to, for hit testing and selection;
/// `locked` is set when the cell's layer is locked, which leaves it out of hit tests.
#[derive(Debug, Clone, PartialEq)]
pub enum Prim {
    /// An outline, rotation already applied to its points.
    Path {
        cell: CellId,
        locked: bool,
        path: Vec<PathCmd>,
        fill: Option<Paint>,
        stroke: Option<Stroke>,
        /// 0–1, applied to fill and stroke together.
        opacity: f64,
        /// Paint [`SHADOW_COLOR`] under it at [`SHADOW_OFFSET`] first.
        shadow: bool,
    },
    /// A label. With `wrap` the runs are wrapped to `rect.w` and the block is aligned inside
    /// `rect` by `align`/`valign`; without it the text is laid out unbounded and placed so that
    /// its (`align`, `valign`) point sits on `anchor`. Either way it is turned `rotation`
    /// degrees about `anchor`.
    Text {
        cell: CellId,
        locked: bool,
        rect: Rect,
        anchor: Point,
        align: Align,
        valign: VAlign,
        wrap: bool,
        rotation: f64,
        font: Font,
        runs: Vec<Run>,
        background: Option<Color>,
        border: Option<Color>,
        opacity: f64,
    },
    /// A picture filling `rect` (keeping its aspect within it when `keep_aspect`), turned
    /// `rotation` degrees about the centre of `rect`.
    Image {
        cell: CellId,
        locked: bool,
        rect: Rect,
        source: ImageSource,
        keep_aspect: bool,
        rotation: f64,
        opacity: f64,
    },
}

impl Prim {
    pub fn cell(&self) -> &str {
        match self {
            Prim::Path { cell, .. } | Prim::Text { cell, .. } | Prim::Image { cell, .. } => cell,
        }
    }

    pub fn locked(&self) -> bool {
        match self {
            Prim::Path { locked, .. } | Prim::Text { locked, .. } | Prim::Image { locked, .. } => {
                *locked
            }
        }
    }

    /// The box it paints into, half the stroke included, for culling and selection.
    pub fn bounds(&self) -> Rect {
        match self {
            Prim::Path { path, stroke, .. } => {
                let r = geom::path_bounds(path).unwrap_or_default();
                r.grow(stroke.as_ref().map_or(0.0, |s| s.width / 2.0))
            }
            Prim::Text {
                rect,
                anchor,
                rotation,
                ..
            } => {
                let corners = [
                    Point::new(rect.x, rect.y),
                    Point::new(rect.right(), rect.y),
                    Point::new(rect.right(), rect.bottom()),
                    Point::new(rect.x, rect.bottom()),
                ];
                geom::bounds_of(corners.map(|p| geom::rotate(p, *anchor, *rotation)))
                    .unwrap_or(*rect)
            }
            Prim::Image { rect, rotation, .. } => geom::bounding_box(rect, *rotation),
        }
    }
}

/// The display list of `page`.
pub fn scene(page: &Page) -> Scene {
    // Placeholder until the builder lands: an empty page of the right size and colour.
    Scene {
        prims: Vec::new(),
        page_size: page.size(),
        background: page.background(),
    }
}
