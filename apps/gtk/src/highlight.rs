//! Markdown styling: core `Span`s (byte ranges) -> `gtk::TextTag`s on the editor buffer.

use accent_core::markdown::{self, Style};
use gtk::prelude::*;
use gtk::{gdk, pango};

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

const TAG_NAMES: &[&str] = &[
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
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
    for (name, scale) in [("h1", 1.6), ("h2", 1.4), ("h3", 1.2), ("h4", 1.1)] {
        let t = tag(name);
        t.set_scale(scale);
        t.set_weight(700);
    }
    for name in ["h5", "h6"] {
        let t = tag(name);
        t.set_weight(700);
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

fn with_alpha(c: gdk::RGBA, alpha: f32) -> gdk::RGBA {
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

/// Re-analyse the whole buffer and re-apply our tags. ~20 ms per MB, so it runs on the main thread
/// behind a debounce rather than on a worker (the analysis needs the text as one contiguous copy).
pub fn apply(buffer: &sourceview5::Buffer) {
    let (start, end) = buffer.bounds();
    let text = buffer.text(&start, &end, true);
    for name in TAG_NAMES {
        buffer.remove_tag_by_name(name, &start, &end);
    }
    let analysis = markdown::analyze(&text);
    let offsets = Offsets::new(&text);
    for span in &analysis.spans {
        let s = buffer.iter_at_offset(offsets.char_of(span.range.start));
        let e = buffer.iter_at_offset(offsets.char_of(span.range.end));
        buffer.apply_tag_by_name(tag_name(span.style), &s, &e);
    }
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
}
