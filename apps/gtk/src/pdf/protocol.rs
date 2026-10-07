//! What crosses the channel between the tab and the thread that renders its document.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use accent_api::PdfLink;
use accent_core::pdf;
use accent_core::search::Options;

use super::cache::{TileKey, Want};

/// Where every note link that highlights a document lands, per page: the quads to paint and the
/// index of the link each came from.
pub type Highlights = HashMap<usize, Vec<(Vec<accent_core::pdf::Rect>, usize)>>;

pub use accent_core::pdf::{NamedInk, fresh_id};

/// One report of a partial eraser across a stroke: the line it moved along, its radius, and the
/// names the widget gave the pieces it worked out would be left, in order.
pub struct Pass {
    pub from: (f32, f32),
    pub to: (f32, f32),
    pub radius: f32,
    pub pieces: Vec<u32>,
}

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
    /// What other readers wrote on one page, answered with its links.
    Comments(usize, Vec<pdf::Comment>),
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
    /// A page was put in, taken out or moved: the document's page sizes again, and the edit, which
    /// says where each page the tab holds anything of went — for an Undo, the inverse of the edit
    /// it took back, a deleted page put back being an insert. Not [`Reply::Reloaded`], which
    /// throws away every render of a document that has been replaced — here every page is still
    /// the page it was, only under another number. `step` names the history's step, the same for
    /// the edit, its Undo and its Redo.
    Repaged {
        sizes: Vec<(f32, f32)>,
        edit: pdf::PageEdit,
        step: u32,
    },
    /// The pages of the PDF called `name` went in, this many of them, or why they could not.
    /// [`Reply::Repaged`] has come first, for the edit they made.
    Imported {
        name: String,
        pages: Result<usize, String>,
    },
    /// Where every note link that highlights this document lands on the page today, and which
    /// link each one is. The whole map every time, so a stale page cannot survive underneath.
    Highlights(Highlights),
    /// An export finished: how many annotations it wrote, or why it could not.
    Exported(Result<usize, String>),
    /// Every ink stroke of one page with its box and style, for the eraser to find and the
    /// Adjust tool to take hold of, and how many erases the thread had answered when it read
    /// them: a list read before an erase still on its way is of the page before it.
    Inks {
        page: usize,
        inks: Vec<NamedInk>,
        erases: u64,
    },
    /// Whether there is anything for Undo to take back and for Redo to put back.
    History {
        undo: bool,
        redo: bool,
    },
    /// This much of this page's annotations changed, in page points, so what is cached of that
    /// part of it is of the old page. A stroke is a few square inches of a page: re-rendering
    /// only the tiles it touches is one tile of work per pen lift rather than a dozen.
    PageChanged(usize, pdf::Rect),
    /// The file now on disk is ours, and this is its etag — which is how the tab tells its own
    /// write from someone else's and does not reload over strokes drawn since.
    Saved(accent_core::fs::Etag),
    /// The drawing could not be written, and why. A read-only file used to swallow every stroke
    /// silently until the tab closed.
    SaveFailed(String),
    /// The document could not be opened at all, with the reason to show in its place.
    Failed(String),
    /// The file was written into under the open document, which reads it as it goes: nothing
    /// more comes from this one until the file is read again.
    Changed,
}

/// Which part of a tab asked for tiles. Each asks again only when its own list changes, so a
/// batch dropped for another asker's newer one was never asked for again: the reading view kept
/// a page blank while the thumbnail strip was being scrolled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asker {
    Reader,
    Strip,
    /// The Ctrl-hover link preview, for the stand-in of the page a link goes to.
    Preview,
}

/// What the render thread is asked for.
pub enum Request {
    /// Visible tiles first, then one viewport of prefetch. A newer batch replaces an older one
    /// from the same view.
    ///
    /// The colours are resolved by the caller, not here: `theme.rs` keeps the chosen theme in
    /// thread-local state, so a render thread asking it would always get the default.
    Tiles {
        from: Asker,
        scale: f32,
        dark: bool,
        theme: pdf::Theme,
        wants: Vec<Want>,
    },
    /// One page's links, and its comments with them.
    Links(usize),
    /// Every page's comments from this page on, for the Outline pane's list. A batch, as a
    /// search is: a newer one replaces it, and anything else is a detour it resumes after.
    Comments(usize),
    /// The glyphs of one page, so text on it can be selected.
    Text(usize),
    Outline,
    Search {
        query: u64,
        text: String,
        /// The find bar's Match Case and Match Whole Word.
        options: Options,
        /// The page the search starts on, the one being read, so its first hit is found first.
        /// It walks on to the end and wraps round to the page before this one.
        from: usize,
        /// How many pages from `from` have been looked at. A query the reader interrupted comes
        /// back with this moved on, so it finishes the document instead of stopping where it was
        /// pushed aside.
        walked: usize,
    },
    /// Where the note links that highlight this document land on the page today.
    Highlights(Vec<PdfLink>),
    /// Write those links into the file as real `/Highlight` annotations, in `color`.
    Export {
        links: Vec<PdfLink>,
        color: [u8; 3],
    },
    /// Write the document as it now stands, with those links as `/Highlight` annotations in
    /// `color`, to `dest`: a copy, the file this thread reads left as it is. `done` hears how it
    /// went.
    Copy {
        links: Vec<PdfLink>,
        color: [u8; 3],
        dest: PathBuf,
        done: Sender<Result<(), String>>,
    },
    /// One free-hand stroke, in that page's own points, drawn the way its tool draws.
    Ink {
        page: usize,
        points: Vec<(f32, f32)>,
        style: pdf::InkStyle,
    },
    /// One shape, drawn the way the pen draws.
    Shape {
        page: usize,
        shape: pdf::Shape,
        style: pdf::InkStyle,
    },
    /// Take one stroke off a page, named by its id — whole, or with `partial` only what that pass
    /// of the eraser covered, the rest kept as pieces of their own. Which stroke the eraser passed
    /// over is decided on the main thread, against the list the tab already holds; `joined` is
    /// that the same drag took one already, which makes the two one step for Undo.
    Erase {
        page: usize,
        id: u32,
        joined: bool,
        partial: Option<Pass>,
    },
    /// Every ink stroke of a page with its box and style, for the eraser and the Adjust tool.
    Inks(usize),
    /// Move or resize one stroke; it comes back at the end of the page's `/Annots`.
    Transform {
        page: usize,
        id: u32,
        matrix: pdf::Matrix,
    },
    /// Put a blank page in, take one out, or move one.
    Pages(pdf::PageEdit),
    /// Put every page of the PDF at `source`, called `name`, in at page `at`, after the last
    /// page for `None`: a PDF dropped onto this one.
    Import {
        at: Option<usize>,
        source: PathBuf,
        name: String,
    },
    /// Take back the last step drawn, erased or moved, or the last page edit, in this session.
    Undo,
    /// Make the last step Undo took back again.
    Redo,
    /// Write the drawn-on document out, if anything was drawn since the last time. The channel,
    /// where there is one, is told when that is done — which is what the window close waits on.
    Save(Option<Sender<()>>),
    Reload,
    /// A rename landed: read and write this path from now on. The document itself is untouched —
    /// a rename moves no bytes — so nothing is re-opened and no tile is thrown away.
    Retarget(PathBuf),
}
