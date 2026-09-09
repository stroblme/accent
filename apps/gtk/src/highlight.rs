//! Editor styling: core `Span`s and CSV `Cell`s (byte ranges) -> `gtk::TextTag`s on the buffer.

use crate::typing;
use accent_core::csv;
use accent_core::markdown::{self, Span, Style};
use gtk::prelude::*;
use gtk::{gdk, pango};
use std::ops::Range;

/// Byte offset -> char offset. `markdown::analyze` reports byte ranges, `TextBuffer` iters count
/// characters, so every span boundary needs translating.
///
/// ponytail: the non-ASCII path builds one `u32` per source byte (4 MB for a 1 MB note) instead of
/// binary-searching a sparse table. Notes are small and this runs once per debounced analysis;
/// switch to a sorted `(byte, char)` table if a multi-megabyte note ever shows up.
pub enum Offsets {
    /// One byte == one char, no table needed.
    Ascii(usize),
    Map(Vec<u32>),
}

impl Offsets {
    pub fn new(text: &str) -> Self {
        if text.is_ascii() {
            return Offsets::Ascii(text.len());
        }
        let mut map = Vec::with_capacity(text.len() + 1);
        let mut chars = 0u32;
        for ch in text.chars() {
            // Continuation bytes map to the char that starts there, so a boundary that lands
            // mid-character clamps to that character's start instead of panicking.
            for _ in 0..ch.len_utf8() {
                map.push(chars);
            }
            chars += 1;
        }
        map.push(chars);
        Offsets::Map(map)
    }

    pub fn char_of(&self, byte: usize) -> i32 {
        match self {
            Offsets::Ascii(len) => byte.min(*len) as i32,
            Offsets::Map(map) => map[byte.min(map.len() - 1)] as i32,
        }
    }
}

/// Tag name for a style. Also the set of tags we own: everything else in the table is left alone.
fn tag_name(style: Style) -> &'static str {
    match style {
        Style::Heading(1) => "h1",
        Style::Heading(2) => "h2",
        Style::Heading(3) => "h3",
        Style::Heading(4) => "h4",
        Style::Heading(5) => "h5",
        Style::Heading(_) => "h6",
        Style::Emphasis => "em",
        Style::Strong => "strong",
        Style::Strikethrough => "strike",
        Style::CodeInline => "code",
        Style::CodeBlock => "codeblock",
        Style::Link => "link",
        Style::WikiLink => "wikilink",
        Style::Image => "image",
        Style::Tag => "tag",
        Style::Quote => "quote",
        Style::ListMarker => "listmarker",
        Style::TaskMarker { checked: true } => "taskdone",
        Style::TaskMarker { checked: false } => "task",
        Style::Math => "math",
        Style::Html => "html",
        Style::Frontmatter => "frontmatter",
        Style::Marker => "marker",
    }
}

/// The heading tags, `h1` first: what a caller that asks "is this a heading" looks up, so the
/// names live here with the tags themselves rather than as literals at every such site.
pub const HEADING_TAGS: [&str; 6] = ["h1", "h2", "h3", "h4", "h5", "h6"];
/// The fenced-block tag, for the same reason.
pub const CODEBLOCK: &str = "codeblock";

/// Heading scale factors, indexed by level - 1: what [`install_tags`] gives the `h1`..`h4` tags,
/// and the 1.0 that leaves `h5` and `h6` at the body size. [`hang`] measures the markers with
/// them, so the two lists cannot drift apart unnoticed.
const HEADING_SCALES: [f64; 6] = [1.6, 1.4, 1.2, 1.1, 1.0, 1.0];

/// How far a wrapped line's hanging indent follows its own gutter. One paragraph tag per content
/// column, because a `GtkTextTag`'s indent is a number of pixels and cannot be a function of the
/// line it lands on; a line indented deeper than this hangs at this column instead, so its wraps
/// still line up under something rather than under nothing.
const WRAP_COLUMNS: usize = 12;

/// The hanging-wrap tags, one per content column and indexed by it. Named after the column they
/// carry so the two cannot drift; [`hang`] gives each one its width.
const WRAP_TAGS: [&str; WRAP_COLUMNS] = [
    "wrap1", "wrap2", "wrap3", "wrap4", "wrap5", "wrap6", "wrap7", "wrap8", "wrap9", "wrap10",
    "wrap11", "wrap12",
];

const TAG_NAMES: &[&str] = &[
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "hang1",
    "hang2",
    "hang3",
    "hang4",
    "hang5",
    "hang6",
    "em",
    "strong",
    "strike",
    "code",
    "codeblock",
    "link",
    "wikilink",
    "image",
    "tag",
    "quote",
    "listmarker",
    "task",
    "taskdone",
    "math",
    "html",
    "frontmatter",
    "marker",
];

/// Create our tags in `buffer`'s tag table (idempotent) and give them their theme-independent
/// attributes. Colours are set separately by [`restyle`] because they follow the system accent.
pub fn install_tags(buffer: &sourceview5::Buffer) {
    let table = buffer.tag_table();
    if table.lookup("marker").is_some() {
        return;
    }
    let tag = |name: &str| {
        let t = gtk::TextTag::new(Some(name));
        table.add(&t);
        t
    };
    // Paragraph tags with nothing visual of their own: `hang` gives them the indent that lines a
    // wrapped line up behind its own marker. Added before the heading tags below, because a tag
    // added later to the table outranks an earlier one and both of these set `indent` — so an
    // indented ATX heading keeps the hang that pulls its `#` markers out.
    for name in WRAP_TAGS {
        tag(name);
    }
    for (name, scale) in [("h1", 1.6), ("h2", 1.4), ("h3", 1.2), ("h4", 1.1)] {
        let t = tag(name);
        t.set_scale(scale);
        t.set_weight(700);
    }
    for name in ["h5", "h6"] {
        let t = tag(name);
        t.set_weight(700);
    }
    // Paragraph tags with nothing visual of their own: `hang` gives them the margins that pull
    // an ATX heading's `#` markers out into the gutter.
    for level in 1..=HEADING_SCALES.len() {
        tag(&format!("hang{level}"));
    }
    tag("strong").set_weight(700);
    tag("em").set_style(pango::Style::Italic);
    tag("strike").set_strikethrough(true);
    for name in ["code", "codeblock", "math", "html", "frontmatter"] {
        tag(name).set_family(Some("monospace"));
    }
    for name in ["link", "wikilink"] {
        tag(name).set_underline(pango::Underline::Single);
    }
    tag("image").set_style(pango::Style::Italic);
    tag("tag");
    let quote = tag("quote");
    quote.set_style(pango::Style::Italic);
    let lm = tag("listmarker");
    lm.set_weight(700);
    tag("task");
    tag("taskdone").set_strikethrough(true);
    tag("marker");
}

/// The colour at `alpha`, which is how everything in the editor dims: composited over the
/// view's background, a foreground at an alpha is the grey the eye reads as "quieter".
pub fn with_alpha(c: gdk::RGBA, alpha: f32) -> gdk::RGBA {
    gdk::RGBA::new(c.red(), c.green(), c.blue(), alpha)
}

/// Apply the current accent colour and foreground-derived dim colours. Call once after the view is
/// realised and again on every `notify::accent-color` / `notify::dark`.
pub fn restyle(buffer: &sourceview5::Buffer, view: &sourceview5::View) {
    let table = buffer.tag_table();
    let accent = adw::StyleManager::default().accent_color_rgba();
    let fg = view.color();
    let set = |name: &str, f: &dyn Fn(&gtk::TextTag)| {
        if let Some(t) = table.lookup(name) {
            f(&t);
        }
    };
    for name in ["link", "wikilink", "tag", "image"] {
        set(name, &|t| t.set_foreground_rgba(Some(&accent)));
    }
    for name in ["marker", "frontmatter", "listmarker"] {
        set(name, &|t| t.set_foreground_rgba(Some(&with_alpha(fg, 0.4))));
    }
    for name in ["quote", "taskdone"] {
        set(name, &|t| t.set_foreground_rgba(Some(&with_alpha(fg, 0.6))));
    }
    for name in ["code", "codeblock"] {
        set(name, &|t| {
            t.set_background_rgba(Some(&with_alpha(fg, 0.07)))
        });
    }
}

/// Re-analyse the whole buffer, re-apply our tags, and hand the analysis back so a caller that
/// also wants the links parses the note once instead of twice. Roughly 0.08 ms on a 2 KB note and
/// 6 ms on half a megabyte, which is what lets short notes restyle on the keystroke; the analysis
/// needs the text as one contiguous copy, so it stays on the main thread either way.
pub fn apply(buffer: &sourceview5::Buffer) -> markdown::Analysis {
    let (start, end) = buffer.bounds();
    let text = buffer.text(&start, &end, true);
    for name in TAG_NAMES.iter().chain(WRAP_TAGS.iter()) {
        buffer.remove_tag_by_name(name, &start, &end);
    }
    let analysis = markdown::analyze(&text);
    let offsets = Offsets::new(&text);
    for span in &analysis.spans {
        tag_span(buffer, &offsets, &text, span);
    }
    // Line by line rather than from the spans: a plain indented line carries no span of its own,
    // and a list marker's span stops short of the space behind it that the text starts after.
    for (n, line) in text.lines().enumerate() {
        let column = typing::wrap_column(line).min(WRAP_COLUMNS);
        if column == 0 {
            continue;
        }
        let Some(s) = buffer.iter_at_line(n as i32) else {
            continue;
        };
        let e = crate::editor::line_end(buffer, n as i32);
        buffer.apply_tag_by_name(WRAP_TAGS[column - 1], &s, &e);
    }
    analysis
}

/// Tag one span, with the hanging indent that pulls an ATX heading's `#` markers out into the
/// gutter where the span is one. Shared with [`apply_line`], which hands it spans clipped to a
/// single line: clipping can only move a start past the `#` markers, and a line that has not got
/// them has nothing to hang.
fn tag_span(buffer: &sourceview5::Buffer, offsets: &Offsets, text: &str, span: &Span) {
    let s = buffer.iter_at_offset(offsets.char_of(span.range.start));
    let e = buffer.iter_at_offset(offsets.char_of(span.range.end));
    buffer.apply_tag_by_name(tag_name(span.style), &s, &e);
    if let Style::Heading(level) = span.style
        && is_atx(text, span.range.start)
    {
        buffer.apply_tag_by_name(&format!("hang{}", level.clamp(1, 6)), &s, &e);
    }
}

/// The bytes of `line` in `text`, its newline excluded, or `None` past the last line.
fn line_bytes(text: &str, line: usize) -> Option<Range<usize>> {
    let mut start = 0;
    let mut lines = 0;
    for piece in text.split_inclusive('\n') {
        if lines == line {
            return Some(start..start + piece.strip_suffix('\n').unwrap_or(piece).len());
        }
        start += piece.len();
        lines += 1;
    }
    // The two lines `split_inclusive` does not yield and a `GtkTextBuffer` still counts: the empty
    // one after a trailing newline, and the single line of an empty buffer.
    (line == lines && (text.is_empty() || text.ends_with('\n'))).then_some(start..start)
}

/// Every span that overlaps `range`, clipped to it, in the order [`apply`] would tag them. A span
/// that *encloses* the range survives across its full width, which is how a line inside a fence or
/// frontmatter keeps the block's styling.
fn clipped(spans: &[Span], range: &Range<usize>) -> Vec<Span> {
    spans
        .iter()
        .filter(|s| s.range.start < range.end && s.range.end > range.start)
        .map(|s| Span {
            range: s.range.start.max(range.start)..s.range.end.min(range.end),
            style: s.style,
        })
        .collect()
}

/// Re-tag one line from a fresh analysis of the whole document, for the long notes that cannot
/// afford [`apply`] on the keystroke. The parse is the cheap half of a pass and the tag churn the
/// expensive one, so the line under the caret is styled as it is typed and the rest waits for the
/// debounced full pass. Parsing everything is also what makes it correct: a line inside a fence or
/// frontmatter is covered by the span the parse emits for the block, so nothing is read out of
/// context.
pub fn apply_line(buffer: &sourceview5::Buffer, line: i32) {
    let (start, end) = buffer.bounds();
    let text = buffer.text(&start, &end, true);
    let Some(bytes) = usize::try_from(line)
        .ok()
        .and_then(|n| line_bytes(&text, n))
    else {
        return;
    };
    let analysis = markdown::analyze(&text);
    let offsets = Offsets::new(&text);
    let s = buffer.iter_at_offset(offsets.char_of(bytes.start));
    let e = buffer.iter_at_offset(offsets.char_of(bytes.end));
    for name in TAG_NAMES.iter().chain(WRAP_TAGS.iter()) {
        buffer.remove_tag_by_name(name, &s, &e);
    }
    for span in clipped(&analysis.spans, &bytes) {
        tag_span(buffer, &offsets, &text, &span);
    }
    let column = typing::wrap_column(&text[bytes]).min(WRAP_COLUMNS);
    if column > 0 {
        buffer.apply_tag_by_name(WRAP_TAGS[column - 1], &s, &e);
    }
}

/// Whether the heading starting at byte `start` writes its own `#` markers *and* has closed them
/// with a space. A setext heading is underlined on the line below instead, so it has no marker to
/// hang. The space matters while typing: a bare `#` still parses as an empty heading, so hanging
/// it would pull the line left the moment the key is pressed and push it back as soon as the next
/// character turns it into a `#tag`.
fn is_atx(text: &str, start: usize) -> bool {
    let rest = &text[start..];
    let after = rest.trim_start_matches('#');
    after.len() < rest.len() && after.starts_with([' ', '\t'])
}

/// Pull each ATX heading's `#` markers out into the left gutter, so heading text lines up with
/// body text the way it does in Apostrophe. The markers are measured rather than guessed: their
/// width follows the document font, the heading's scale and the current zoom.
///
/// Pango reads a negative indent as a *hanging* indent, so the first line of the paragraph sits
/// at the tag's left margin and the wrapped ones at `left_margin + |indent|`; a tag's left margin
/// replaces the view's rather than adding to it. Putting the marker's width in both therefore
/// starts the heading line that much left of the gutter and lands everything after the marker,
/// wrapped lines included, on the body column.
///
/// ponytail: the 48 px gutter is not widened, so `##### ` and `###### ` are wider than it, clamp
/// at the window edge and start their text a few pixels right of the body column. Widen the
/// gutter, or scale it with the zoom, if that ever reads as a misalignment.
pub fn hang(buffer: &sourceview5::Buffer, view: &sourceview5::View) {
    let table = buffer.tag_table();
    let gutter = view.left_margin();
    for (level, scale) in HEADING_SCALES.iter().enumerate() {
        let Some(tag) = table.lookup(&format!("hang{}", level + 1)) else {
            continue;
        };
        // The layout inherits the view's font from its pango context, so whatever CSS says about
        // the document font and the zoom is already in it; the two attributes are exactly what
        // the matching `h{n}` tag adds on top of it.
        let layout = view.create_pango_layout(Some(&format!("{} ", "#".repeat(level + 1))));
        let attrs = pango::AttrList::new();
        attrs.insert(pango::AttrInt::new_weight(pango::Weight::Bold));
        attrs.insert(pango::AttrFloat::new_scale(*scale));
        layout.set_attributes(Some(&attrs));
        let width = layout.pixel_size().0;
        tag.set_left_margin((gutter - width).max(0));
        tag.set_indent(-width.min(gutter));
    }
    // The wrapped-line indents, the same hanging trick with the left margin left alone: the first
    // line stays where it was and only the wraps step in, behind the line's own indent and list
    // marker. One character's advance times the column, which is exact in the monospaced face a
    // note is written in (`editor::DEFAULT_FAMILY`) and an average in a proportional one.
    let em = view
        .pango_context()
        .metrics(None, None)
        .approximate_char_width()
        / pango::SCALE;
    for (column, name) in WRAP_TAGS.iter().enumerate() {
        if let Some(tag) = table.lookup(name) {
            tag.set_indent(-em * (column as i32 + 1));
        }
    }
}

/// How many hues the columns cycle through before repeating.
pub const CSV_COLUMNS: usize = 6;

/// The CSV column tags, deliberately outside [`TAG_NAMES`]: the two sets never overlap, so
/// neither one's remove pass can reach the other's tags.
const CSV_TAG_NAMES: [&str; CSV_COLUMNS] = ["csv0", "csv1", "csv2", "csv3", "csv4", "csv5"];

/// Install the column tags in `buffer`'s tag table (idempotent). Colours come from
/// [`restyle_csv`], as the markdown tags' do from [`restyle`].
pub fn install_csv_tags(buffer: &sourceview5::Buffer) {
    let table = buffer.tag_table();
    if table.lookup(CSV_TAG_NAMES[0]).is_some() {
        return;
    }
    for name in CSV_TAG_NAMES {
        table.add(&gtk::TextTag::new(Some(name)));
    }
}

/// Tag every cell with its column's tag, replacing whatever was there. A file that ends without a
/// row terminator still has its last cell tagged, and an empty buffer yields no cells at all, so
/// both are the same loop over nothing special.
pub fn apply_csv(buffer: &sourceview5::Buffer) {
    let (start, end) = buffer.bounds();
    let text = buffer.text(&start, &end, true);
    for name in CSV_TAG_NAMES {
        buffer.remove_tag_by_name(name, &start, &end);
    }
    let offsets = Offsets::new(&text);
    for cell in csv::columns(&text) {
        let s = buffer.iter_at_offset(offsets.char_of(cell.range.start));
        let e = buffer.iter_at_offset(offsets.char_of(cell.range.end));
        buffer.apply_tag_by_name(CSV_TAG_NAMES[cell.column % CSV_COLUMNS], &s, &e);
    }
}

/// Give the column tags their colours, derived from the current accent. Call it where [`restyle`]
/// is called: the palette follows the system accent and the columns have no other colour source.
pub fn restyle_csv(buffer: &sourceview5::Buffer) {
    let table = buffer.tag_table();
    let accent = adw::StyleManager::default().accent_color_rgba();
    let hsv = gtk::rgb_to_hsv(accent.red(), accent.green(), accent.blue());
    for (column, name) in CSV_TAG_NAMES.iter().enumerate() {
        let Some(tag) = table.lookup(name) else {
            continue;
        };
        let (h, s, v) = rotate(hsv, column);
        let (r, g, b) = gtk::hsv_to_rgb(h, s, v);
        tag.set_foreground_rgba(Some(&gdk::RGBA::new(r, g, b, accent.alpha())));
    }
}

/// A git graph lane's colour, on the same wheel the CSV columns use (DESIGN.md, Colour): the
/// accent's hue turned `column` sixths of a turn, so lane 0 is the accent and a seventh lane
/// repeats the first hue instead of inventing a colour.
pub fn lane_colour(column: usize) -> gdk::RGBA {
    let accent = adw::StyleManager::default().accent_color_rgba();
    let hsv = gtk::rgb_to_hsv(accent.red(), accent.green(), accent.blue());
    let (h, s, v) = rotate(hsv, column % CSV_COLUMNS);
    let (r, g, b) = gtk::hsv_to_rgb(h, s, v);
    gdk::RGBA::new(r, g, b, accent.alpha())
}

/// The accent's hue moved `column` sixths of a turn around the wheel, saturation and value
/// untouched. Column 0 is the accent itself, which is what makes the six read as one family
/// rather than as a second palette.
pub(crate) fn rotate(hsv: (f32, f32, f32), column: usize) -> (f32, f32, f32) {
    let (h, s, v) = hsv;
    ((h + column as f32 / CSV_COLUMNS as f32).fract(), s, v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_offsets_are_identity() {
        let o = Offsets::new("# hello");
        assert!(matches!(o, Offsets::Ascii(7)));
        assert_eq!(o.char_of(0), 0);
        assert_eq!(o.char_of(7), 7);
        assert_eq!(o.char_of(99), 7, "past the end clamps");
    }

    #[test]
    fn multibyte_offsets_map_to_chars() {
        // 'a'=1 byte, 'é'=2, '中'=3, 'b'=1  ->  7 bytes, 4 chars
        let text = "aé中b";
        assert_eq!(text.len(), 7);
        let o = Offsets::new(text);
        assert_eq!(o.char_of(0), 0);
        assert_eq!(o.char_of(1), 1); // start of 'é'
        assert_eq!(o.char_of(2), 1); // continuation byte clamps to 'é'
        assert_eq!(o.char_of(3), 2); // start of '中'
        assert_eq!(o.char_of(5), 2);
        assert_eq!(o.char_of(6), 3); // 'b'
        assert_eq!(o.char_of(7), 4); // end of text
        assert_eq!(o.char_of(999), 4);
    }

    #[test]
    fn every_style_has_a_tag_we_install() {
        let styles = [
            Style::Heading(1),
            Style::Heading(6),
            Style::Heading(9),
            Style::Emphasis,
            Style::Strong,
            Style::Strikethrough,
            Style::CodeInline,
            Style::CodeBlock,
            Style::Link,
            Style::WikiLink,
            Style::Image,
            Style::Tag,
            Style::Quote,
            Style::ListMarker,
            Style::TaskMarker { checked: true },
            Style::TaskMarker { checked: false },
            Style::Math,
            Style::Html,
            Style::Frontmatter,
            Style::Marker,
        ];
        for s in styles {
            assert!(
                TAG_NAMES.contains(&tag_name(s)),
                "{s:?} maps to an uninstalled tag"
            );
        }
    }

    /// Only a heading that carries its own `#` markers has anything to hang in the gutter.
    #[test]
    fn only_atx_headings_hang() {
        let text = "# Head\n\nSetext\n======\n";
        let a = markdown::analyze(text);
        let starts: Vec<usize> = a
            .spans
            .iter()
            .filter(|s| matches!(s.style, Style::Heading(_)))
            .map(|s| s.range.start)
            .collect();
        assert_eq!(starts.len(), 2, "one ATX and one setext heading");
        assert!(is_atx(text, starts[0]));
        assert!(!is_atx(text, starts[1]));
        // A heading is only a heading once the space is there; until then the line may still turn
        // into a tag, and a marker that hangs and un-hangs per keystroke is worse than one that waits.
        assert!(!is_atx("#", 0));
        assert!(!is_atx("#tag", 0));
        assert!(is_atx("### Deep", 0));
    }

    /// The wrap tags are indexed by the column they carry, so a typo in the list would silently
    /// hang a line at the wrong depth.
    #[test]
    fn the_wrap_tags_are_named_after_their_columns() {
        for (i, name) in WRAP_TAGS.iter().enumerate() {
            assert_eq!(*name, format!("wrap{}", i + 1));
        }
    }

    /// The span offsets we feed to the buffer must land on real character boundaries for a note
    /// with multibyte text, otherwise GTK would silently mis-tag.
    #[test]
    fn analysis_offsets_translate_within_bounds() {
        let text = "# Überschrift\n\nEin **fetter** Tag #zettel und [[Ziel|Änderung]].\n";
        let a = markdown::analyze(text);
        let o = Offsets::new(text);
        let chars = text.chars().count() as i32;
        assert!(!a.spans.is_empty());
        for span in &a.spans {
            let (s, e) = (o.char_of(span.range.start), o.char_of(span.range.end));
            assert!(s <= e && e <= chars, "{span:?} -> {s}..{e} of {chars}");
        }
    }

    /// A line's bytes without its newline, including the two lines `split_inclusive` cannot see:
    /// the empty one a `GtkTextBuffer` reports after a trailing newline, and the one line an
    /// empty buffer has.
    #[test]
    fn a_line_is_its_own_bytes_without_the_newline() {
        assert_eq!(line_bytes("a\nb\n", 0), Some(0..1));
        assert_eq!(line_bytes("a\nb\n", 1), Some(2..3));
        assert_eq!(
            line_bytes("a\nb\n", 2),
            Some(4..4),
            "the empty line after the last newline"
        );
        assert_eq!(line_bytes("a\nb\n", 3), None);
        assert_eq!(line_bytes("a\nb", 1), Some(2..3), "no trailing newline");
        assert_eq!(line_bytes("a\nb", 2), None);
        assert_eq!(
            line_bytes("a\n\nb", 1),
            Some(2..2),
            "an empty line in the middle"
        );
        assert_eq!(
            line_bytes("", 0),
            Some(0..0),
            "an empty buffer still has line 0"
        );
        assert_eq!(line_bytes("", 1), None);
    }

    /// What [`apply_line`] tags: everything the line touches, clipped to it. The enclosing case is
    /// the one that matters — a line inside a fence keeps the block's span across its full width,
    /// which is how a whole-document parse gives a per-line pass its context.
    #[test]
    fn clipping_keeps_every_span_the_line_touches() {
        let span = |range: Range<usize>| Span {
            range,
            style: Style::CodeBlock,
        };
        let spans = [
            span(12..15),
            span(5..12),
            span(18..25),
            span(0..100),
            span(25..30),
            span(5..10),
            span(20..25),
        ];
        let got: Vec<Range<usize>> = clipped(&spans, &(10..20))
            .into_iter()
            .map(|s| s.range)
            .collect();
        assert_eq!(got, vec![12..15, 10..12, 18..20, 10..20]);
    }

    /// The six column hues: distinct, a sixth of the wheel apart, wrapping once past 1.0, with the
    /// accent's saturation and value carried through untouched. `rotate` is the whole colour rule
    /// that does not need GTK, and GTK cannot be initialised in a test here.
    #[test]
    fn csv_hues_step_evenly_around_the_wheel() {
        let accent = (0.75, 0.4, 0.9);
        let hues: Vec<f32> = (0..CSV_COLUMNS)
            .map(|column| {
                let (h, s, v) = rotate(accent, column);
                assert_eq!(
                    (s, v),
                    (accent.1, accent.2),
                    "column {column} changed s or v"
                );
                assert!((0.0..1.0).contains(&h), "hue {h} left the wheel");
                h
            })
            .collect();
        assert_eq!(hues[0], accent.0, "column 0 is the accent itself");
        assert!(hues[2] < hues[1], "the third column has wrapped past 1.0");
        let mut distinct = hues.clone();
        distinct.sort_by(f32::total_cmp);
        distinct.dedup();
        assert_eq!(distinct.len(), CSV_COLUMNS);
        for pair in hues.windows(2) {
            let step = (pair[1] - pair[0]).rem_euclid(1.0);
            assert!((step - 1.0 / CSV_COLUMNS as f32).abs() < 1e-6, "{pair:?}");
        }
    }
}
