//! What the types the façade speaks look like on the other side.
//!
//! Three things do not cross and are restated here. uniffi carries no `usize`, no
//! `std::ops::Range`, no `PathBuf`, no tuple and no `char`, so anything holding one gets a
//! record of its own. A colour crosses as one `u32` rather than an array of bytes, which is
//! also how Android already spells one. And a text offset changes meaning: the core counts
//! bytes, Kotlin counts UTF-16 units, so every range over a document's text is converted on the
//! way out (see [`Utf16`]).
//!
//! Everything else is declared to uniffi where it already stands, with `#[uniffi::remote]`: no
//! second definition to keep in step, no conversion at all.

// Every name here is imported bare because `#[uniffi::remote]` restates the type where it
// already stands: the name has to resolve to the real one.
use accent_core::fs::Etag;
use accent_core::index::{self, Backlink, FileRow, Phase};
use accent_core::markdown::{LinkKind, Style};
use accent_core::pdf::{self, Rect, SelectionLink};
use accent_core::walk::FileKind;

// ------------------------------------------------------------------ what already crosses as is

#[uniffi::remote(Record)]
pub struct Etag {
    pub mtime_ns: i64,
    pub size: u64,
    pub ino: u64,
}

#[uniffi::remote(Enum)]
pub enum FileKind {
    Dir,
    Markdown,
    Pdf,
    Other,
    Conflict,
}

#[uniffi::remote(Record)]
pub struct FileRow {
    pub id: i64,
    pub rel_path: String,
    pub kind: FileKind,
    pub title: Option<String>,
    pub size: i64,
    pub mtime_ns: i64,
}

#[uniffi::remote(Record)]
pub struct Backlink {
    pub src_rel_path: String,
    pub byte_start: i64,
    pub byte_end: i64,
}

#[uniffi::remote(Enum)]
pub enum Phase {
    Scan,
    Index,
    Resolve,
}

#[uniffi::remote(Record)]
pub struct Rect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

#[uniffi::remote(Record)]
pub struct SelectionLink {
    pub link: String,
    pub quads: Vec<pdf::Rect>,
    pub text: String,
}

#[uniffi::remote(Enum)]
pub enum Style {
    Heading(u8),
    Emphasis,
    Strong,
    Strikethrough,
    CodeInline,
    CodeBlock,
    Link,
    WikiLink,
    Image,
    Tag,
    Quote,
    ListMarker,
    TaskMarker { checked: bool },
    Math,
    Html,
    Frontmatter,
    Marker,
}

#[uniffi::remote(Enum)]
pub enum LinkKind {
    Wiki,
    Embed,
    Markdown,
    External,
}

// ------------------------------------------------------------------------------- text offsets

/// Byte offset in a Rust string → UTF-16 code-unit offset in the Kotlin string it becomes.
///
/// One entry per byte plus the end. Every byte of a character maps to the unit its character
/// starts at, so a range that somehow cuts a character in half clamps instead of lying.
pub struct Utf16(Vec<u32>);

impl Utf16 {
    pub fn new(text: &str) -> Utf16 {
        let mut map = vec![0u32; text.len() + 1];
        let mut units = 0u32;
        for (at, ch) in text.char_indices() {
            for slot in &mut map[at..at + ch.len_utf8()] {
                *slot = units;
            }
            units += ch.len_utf16() as u32;
        }
        map[text.len()] = units;
        Utf16(map)
    }

    pub fn at(&self, byte: usize) -> u32 {
        self.0
            .get(byte)
            .copied()
            .unwrap_or_else(|| self.0.last().copied().unwrap_or(0))
    }

    pub fn range(&self, r: &std::ops::Range<usize>) -> Range {
        Range {
            start: self.at(r.start),
            end: self.at(r.end),
        }
    }
}

/// Half-open, in UTF-16 code units unless the field that carries it says otherwise.
#[derive(uniffi::Record)]
pub struct Range {
    pub start: u32,
    pub end: u32,
}

// ------------------------------------------------------------------------------ vault records

/// A note as it was read: the text and the stamp a save has to be made against.
#[derive(uniffi::Record)]
pub struct Note {
    pub text: String,
    pub etag: Etag,
}

/// A note a template made: its text and where the template asked for the caret, in UTF-16 units.
#[derive(uniffi::Record)]
pub struct NewNote {
    pub text: String,
    pub carets: Vec<u32>,
}

#[derive(uniffi::Record)]
pub struct TagCount {
    pub name: String,
    pub count: i64,
}

#[derive(uniffi::Record)]
pub struct SearchHit {
    pub rel_path: String,
    pub title: Option<String>,
    pub snippet: String,
    /// Byte range of the phrase in the note — bytes, not UTF-16 units, because the note it
    /// points into has not been read yet. A caller that wants to place a caret converts it
    /// against the text it then reads.
    pub at: Option<Range>,
}

/// A note link pointing into a page of a PDF: what paints as a highlight over that page.
#[derive(uniffi::Record)]
pub struct PdfLink {
    pub src_rel_path: String,
    pub byte_start: i64,
    pub page: u32,
    /// The four numbers the link spells, in order.
    pub selection: Vec<u32>,
    pub alias: Option<String>,
}

#[derive(uniffi::Record)]
pub struct Progress {
    pub phase: Phase,
    pub done: u64,
    pub total: u64,
}

// ---------------------------------------------------------------------------- markdown records

#[derive(uniffi::Record)]
pub struct Span {
    pub range: Range,
    pub style: Style,
}

#[derive(uniffi::Record)]
pub struct Link {
    pub range: Range,
    pub kind: LinkKind,
    pub target: String,
    pub anchor: Option<String>,
    pub alias: Option<String>,
}

#[derive(uniffi::Record)]
pub struct Tag {
    pub range: Range,
    pub name: String,
}

#[derive(uniffi::Record)]
pub struct Heading {
    pub range: Range,
    pub level: u8,
    pub text: String,
}

#[derive(uniffi::Record)]
pub struct Analysis {
    pub spans: Vec<Span>,
    pub links: Vec<Link>,
    pub tags: Vec<Tag>,
    pub headings: Vec<Heading>,
    pub title: Option<String>,
    pub frontmatter: Option<String>,
}

// --------------------------------------------------------------------------------- pdf records

/// A rendered piece of a page: tightly packed RGBA8, `width * height * 4` bytes.
#[derive(uniffi::Record)]
pub struct Tile {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// A page's size in points.
#[derive(uniffi::Record)]
pub struct PageSize {
    pub width: f32,
    pub height: f32,
}

/// One character with its box on the page. `ch` is one character, as a string: uniffi has no
/// `char`, and Kotlin has no character that holds an astral one either.
#[derive(uniffi::Record)]
pub struct Glyph {
    pub ch: String,
    pub rect: pdf::Rect,
    pub index: u32,
}

/// How a stroke is drawn. `rgba` is `0xRRGGBBAA`.
#[derive(uniffi::Record)]
pub struct InkStyle {
    pub width: f32,
    pub rgba: u32,
    /// A highlighter darkens what is under it instead of covering it.
    pub multiply: bool,
}

/// How a page is recoloured on its way to the screen. Colours are `0xRRGGBB`.
#[derive(uniffi::Enum)]
pub enum Theme {
    Plain,
    Recolour { paper: u32, ink: u32 },
}

#[derive(uniffi::Enum)]
pub enum LinkTarget {
    Page { page: u32, top: Option<f32> },
    Uri { uri: String },
}

#[derive(uniffi::Record)]
pub struct PdfLinkBox {
    pub rect: pdf::Rect,
    pub target: LinkTarget,
}

#[derive(uniffi::Record)]
pub struct Outline {
    pub depth: u32,
    pub title: String,
    pub page: Option<u32>,
}

/// An existing `/Highlight` annotation read out of the document. `color` is `0xRRGGBBAA`.
#[derive(uniffi::Record)]
pub struct Highlight {
    pub page: u32,
    pub quads: Vec<pdf::Rect>,
    pub color: u32,
    pub contents: Option<String>,
}

/// A point in page points, origin top-left.
#[derive(uniffi::Record)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// How much of which page a change touched, so a viewer knows which tiles to draw again.
#[derive(uniffi::Record)]
pub struct PageArea {
    pub page: u32,
    pub area: pdf::Rect,
}

/// Whether Undo, and then Redo, has anything to walk.
#[derive(uniffi::Record, Debug, PartialEq, Eq)]
pub struct History {
    pub undo: bool,
    pub redo: bool,
}

/// One line of a search hit on a page.
#[derive(uniffi::Record)]
pub struct SearchLine {
    pub quads: Vec<pdf::Rect>,
}

// -------------------------------------------------------------------------------- conversions

/// The one place a `Vec` of anything becomes a `Vec` of what it converts to.
pub fn all<T: Into<U>, U>(v: Vec<T>) -> Vec<U> {
    v.into_iter().map(Into::into).collect()
}

fn rgba(c: [u8; 4]) -> u32 {
    u32::from_be_bytes(c)
}

impl From<index::Progress> for Progress {
    fn from(p: index::Progress) -> Self {
        Progress {
            phase: p.phase,
            done: p.done as u64,
            total: p.total as u64,
        }
    }
}

impl From<index::SearchHit> for SearchHit {
    fn from(h: index::SearchHit) -> Self {
        SearchHit {
            rel_path: h.rel_path,
            title: h.title,
            snippet: h.snippet,
            at: h.at.map(|r| Range {
                start: r.start as u32,
                end: r.end as u32,
            }),
        }
    }
}

impl From<index::PdfLink> for PdfLink {
    fn from(l: index::PdfLink) -> Self {
        PdfLink {
            src_rel_path: l.src_rel_path,
            byte_start: l.byte_start,
            page: l.page as u32,
            selection: l.selection.iter().map(|n| *n as u32).collect(),
            alias: l.alias,
        }
    }
}

impl From<pdf::RgbaImage> for Tile {
    fn from(i: pdf::RgbaImage) -> Self {
        Tile {
            width: i.width,
            height: i.height,
            rgba: i.data,
        }
    }
}

impl From<(f32, f32)> for PageSize {
    fn from((width, height): (f32, f32)) -> Self {
        PageSize { width, height }
    }
}

impl From<pdf::Glyph> for Glyph {
    fn from(g: pdf::Glyph) -> Self {
        Glyph {
            ch: g.ch.to_string(),
            rect: g.rect,
            index: g.index as u32,
        }
    }
}

impl From<InkStyle> for pdf::InkStyle {
    fn from(s: InkStyle) -> Self {
        pdf::InkStyle {
            width: s.width,
            rgba: s.rgba.to_be_bytes(),
            multiply: s.multiply,
        }
    }
}

impl From<Theme> for pdf::Theme {
    fn from(t: Theme) -> Self {
        match t {
            Theme::Plain => pdf::Theme::Plain,
            Theme::Recolour { paper, ink } => {
                let three = |c: u32| {
                    let [_, r, g, b] = c.to_be_bytes();
                    [r, g, b]
                };
                pdf::Theme::Recolour {
                    paper: three(paper),
                    ink: three(ink),
                }
            }
        }
    }
}

impl From<pdf::Link> for PdfLinkBox {
    fn from(l: pdf::Link) -> Self {
        PdfLinkBox {
            rect: l.rect,
            target: match l.target {
                pdf::LinkTarget::Page { page, top } => LinkTarget::Page {
                    page: page as u32,
                    top,
                },
                pdf::LinkTarget::Uri(uri) => LinkTarget::Uri { uri },
            },
        }
    }
}

impl From<pdf::Outline> for Outline {
    fn from(o: pdf::Outline) -> Self {
        Outline {
            depth: o.depth as u32,
            title: o.title,
            page: o.page.map(|p| p as u32),
        }
    }
}

impl From<pdf::Highlight> for Highlight {
    fn from(h: pdf::Highlight) -> Self {
        Highlight {
            page: h.page as u32,
            quads: h.quads,
            color: rgba(h.color),
            contents: h.contents,
        }
    }
}

impl From<Point> for (f32, f32) {
    fn from(p: Point) -> Self {
        (p.x, p.y)
    }
}

impl From<(usize, pdf::Rect)> for PageArea {
    fn from((page, area): (usize, pdf::Rect)) -> Self {
        PageArea {
            page: page as u32,
            area,
        }
    }
}

impl From<Vec<pdf::Rect>> for SearchLine {
    fn from(quads: Vec<pdf::Rect>) -> Self {
        SearchLine { quads }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Kotlin counts UTF-16 units, so a note with anything above ASCII in it has to be measured
    /// twice: once for the text the core parsed and once for the string Android holds.
    #[test]
    fn a_range_crosses_in_the_units_the_other_side_counts() {
        // "é" is two bytes and one unit; "🙂" is four bytes and two units.
        let text = "é🙂ab";
        let map = Utf16::new(text);
        assert_eq!(map.at(0), 0);
        assert_eq!(map.at(2), 1, "after é");
        assert_eq!(map.at(6), 3, "after 🙂");
        assert_eq!(map.at(text.len()), 5, "the whole string");
        // A byte inside a character clamps to where that character starts.
        assert_eq!(map.at(1), 0);
        assert_eq!(map.at(4), 1);

        let r = map.range(&(2..6));
        assert_eq!((r.start, r.end), (1, 3));
        // Past the end is the end, not a panic.
        assert_eq!(map.at(text.len() + 10), 5);
        assert_eq!(Utf16::new("").at(0), 0);
    }

    /// A colour is one number on the other side, and it has to survive the trip both ways.
    #[test]
    fn a_colour_crosses_as_one_number() {
        assert_eq!(rgba([0x11, 0x22, 0x33, 0x44]), 0x1122_3344);
        let style = InkStyle {
            width: 2.0,
            rgba: 0x1122_3344,
            multiply: true,
        };
        let core: pdf::InkStyle = style.into();
        assert_eq!(core.rgba, [0x11, 0x22, 0x33, 0x44]);
        assert!(core.multiply);

        let theme: pdf::Theme = Theme::Recolour {
            paper: 0x00ff_eedd,
            ink: 0x0011_2233,
        }
        .into();
        assert_eq!(
            theme,
            pdf::Theme::Recolour {
                paper: [0xff, 0xee, 0xdd],
                ink: [0x11, 0x22, 0x33]
            }
        );
    }

    /// The four numbers of a PDF link keep their order when the array becomes a list.
    #[test]
    fn a_pdf_links_four_numbers_keep_their_order() {
        let link: PdfLink = accent_core::index::PdfLink {
            src_rel_path: "Note.md".to_string(),
            byte_start: 7,
            page: 2,
            selection: [4, 0, 4, 11],
            alias: Some("quoted".to_string()),
        }
        .into();
        assert_eq!(link.selection, vec![4, 0, 4, 11]);
        assert_eq!(link.page, 2);
    }
}
