//! What crosses the channel between the tab and the thread that renders its document.

use std::collections::HashMap;

use super::cache::TileKey;

/// Where every note link that highlights a document lands, per page: the quads to paint and the
/// index of the link each came from.
pub type Highlights = HashMap<usize, Vec<(Vec<accent_core::pdf::Rect>, usize)>>;

/// What the render thread sends back. Every variant is `Send`, because each one travels to the
/// main loop inside its own idle callback.
pub enum Reply {
    Tile(TileKey, accent_core::pdf::RgbaImage),
    Lowres {
        page: u32,
        dark: bool,
        image: accent_core::pdf::RgbaImage,
    },
    Links(usize, Vec<accent_core::pdf::Link>),
    /// One page's glyphs and their boxes, for selecting text on it.
    Text(usize, Vec<accent_core::pdf::Glyph>),
    Outline(Vec<accent_core::pdf::Outline>),
    /// One page's matches for the query identified by `query`; a later one abandons it.
    Found {
        query: u64,
        page: usize,
        hits: Vec<Vec<accent_core::pdf::Rect>>,
    },
    /// The file was read: these are its page sizes. The first one arrives when the document is
    /// opened, which is why a tab can be on screen before anything is known about it.
    Reloaded(Vec<(f32, f32)>),
    /// Where every note link that highlights this document lands on the page today, and which
    /// link each one is. The whole map every time, so a stale page cannot survive underneath.
    Highlights(Highlights),
    /// An export finished: how many annotations it wrote, or why it could not.
    Exported(Result<usize, String>),
    /// Every ink stroke of one page with its box and style, for the Adjust tool to take hold of.
    Inks {
        page: usize,
        inks: Vec<accent_core::pdf::InkShape>,
    },
    /// This page's annotations changed, so what is cached of it is of the old page.
    PageChanged(usize),
    /// The file now on disk is ours, and this is its etag — which is how the tab tells its own
    /// write from someone else's and does not reload over strokes drawn since.
    Saved(accent_core::fs::Etag),
    /// The document could not be opened at all, with the reason to show in its place.
    Failed(String),
}
