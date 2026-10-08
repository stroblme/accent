//! Editor styling: core `Span`s and CSV `Cell`s (byte ranges) -> `gtk::TextTag`s on the buffer.

use crate::theme;
use accent_core::csv;
use accent_core::markdown::{self, Span, Style};
use gtk::prelude::*;
use gtk::{gdk, pango};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

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
/// The paragraph tags [`hang`] gives each heading level's margins, `hang1` first.
const HANG_TAGS: [&str; 6] = ["hang1", "hang2", "hang3", "hang4", "hang5", "hang6"];
/// The fenced-block tag, for the same reason.
pub const CODEBLOCK: &str = "codeblock";

/// Heading scale factors, indexed by level - 1: what [`install_tags`] gives the `h1`..`h4` tags,
/// and the 1.0 that leaves `h5` and `h6` at the body size. [`hang`] measures the markers with
/// them, so the two lists cannot drift apart unnoticed.
const HEADING_SCALES: [f64; 6] = [1.6, 1.4, 1.2, 1.1, 1.0, 1.0];

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
/// After `wrap::install`, so the heading hangs outrank the wrap indents.
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
    // Paragraph tags with nothing visual of their own: `hang` gives them the margins that pull
    // an ATX heading's `#` markers out into the gutter.
    for level in 1..=HEADING_SCALES.len() {
        tag(&format!("hang{level}"));
    }
    tag("strong").set_weight(700);
    tag("em").set_style(pango::Style::Italic);
    tag("strike").set_strikethrough(true);
    let mono = monospace_family();
    for name in ["code", "codeblock", "math", "html", "frontmatter"] {
        tag(name).set_family(Some(&mono));
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

/// The family of the platform's monospace font, which is the font a code tab is given
/// (`editor::page::install_font`) and the one a run of code or a formula inside a note is set in.
///
/// Named rather than left to CSS's generic `monospace`: the alias is whatever fontconfig answers
/// with, which is a face nothing else in the window uses, and a run set in it sits on the same
/// line as prose set in another — so its glyphs are rasterised on their own terms, at a size
/// chosen for the other font's metrics. That is what left an inline `$=$` with the top bar of its
/// `=` all but gone beside a plain `=` two lines down that kept it. With the defaults this is the
/// note's own face, and the formula is drawn exactly as the text around it is.
fn monospace_family() -> String {
    pango::FontDescription::from_string(&adw::StyleManager::default().monospace_font_name())
        .family()
        .map(|family| family.to_string())
        .unwrap_or_else(|| "monospace".to_string())
}

/// The lowest contrast a dim colour may read at against the page under it.
///
/// The alphas below say what a marker is worth where the page has contrast to spare, and on
/// Adwaita they give 2.8:1 in light and 3.8:1 in dark — which is what every note has been read
/// at. On a page whose own ink is close to it they say nothing: Solarized's prose is 4.1:1
/// against its paper, so 40 % of it is 1.6:1 and the bullet is not there. This is the floor those
/// alphas are held above, so a scheme is lifted into the band rather than needing an alpha of its
/// own.
///
/// A contrast target and not WCAG's 4.5:1 on purpose. That floor is for text somebody reads; a
/// marker is punctuation the eye is meant to pass over, and lifting markup to 4.5:1 would make it
/// louder than the prose in every theme. The number is Adwaita's own dimmer half, so the default
/// theme does not move and nothing here is a judgement about what reads well — only that no page
/// may be quieter than the one people already read.
const DIM_FLOOR: f32 = 2.8;

/// The floor on Solarized alone. Its prose is only 4.13:1 light and 4.75:1 dark, so under
/// [`DIM_FLOOR`] a list marker (0.4) and a quote (0.6) were both lifted onto 2.8:1 and read the
/// same. 1.8 is Adwaita's marker carried over: 2.84:1 under prose at 12.6:1 is 41 % of the
/// prose's contrast on a log scale, and 41 % of 4.13:1 is 1.8:1. The marker lands on 1.80:1 light
/// under a quote at 2.16:1, and dark keeps both alphas, 1.86:1 under 2.59:1.
const SOLARIZED_DIM_FLOOR: f32 = 1.8;

/// What `colour` reads at against `page`, composited over it: a translucent foreground *is* a mix
/// with what is behind it, so that mix is the contrast the reader gets. WCAG 2.1's ratio, which
/// is the only definition of "reads at" anyone shares.
pub fn reads_at(colour: gdk::RGBA, page: gdk::RGBA) -> f32 {
    let a = colour.alpha();
    let over = |c: f32, p: f32| a * c + (1.0 - a) * p;
    let luminance = |r: f32, g: f32, b: f32| {
        let lin = |v: f32| match v <= 0.040_45 {
            true => v / 12.92,
            false => ((v + 0.055) / 1.055).powf(2.4),
        };
        0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
    };
    let mixed = luminance(
        over(colour.red(), page.red()),
        over(colour.green(), page.green()),
        over(colour.blue(), page.blue()),
    );
    let under = luminance(page.red(), page.green(), page.blue());
    (mixed.max(under) + 0.05) / (mixed.min(under) + 0.05)
}

/// `ink` at `alpha`, or at as much more of it as it takes to clear the theme's floor
/// ([`DIM_FLOOR`], [`SOLARIZED_DIM_FLOOR`]) against `page`.
pub fn dim(ink: gdk::RGBA, page: gdk::RGBA, alpha: f32) -> gdk::RGBA {
    let floor = match theme::solarized() {
        true => SOLARIZED_DIM_FLOOR,
        false => DIM_FLOOR,
    };
    lift(ink, page, alpha, floor)
}

/// `ink` at `alpha`, or at as much more of it as it takes to clear `floor` against `page`.
///
/// Contrast against the page only grows as the ink does, so the smallest alpha that clears the
/// floor is a bisection away; a page with no room left hands back the ink itself.
fn lift(ink: gdk::RGBA, page: gdk::RGBA, alpha: f32, floor: f32) -> gdk::RGBA {
    if reads_at(theme::at(ink, alpha), page) >= floor {
        return theme::at(ink, alpha);
    }
    let (mut lo, mut hi) = (alpha, 1.0);
    // Twenty halvings land within 1e-6 of the boundary, far finer than the 1/255 it is painted at.
    for _ in 0..20 {
        let mid = 0.5 * (lo + hi);
        match reads_at(theme::at(ink, mid), page) >= floor {
            true => hi = mid,
            false => lo = mid,
        }
    }
    theme::at(ink, hi)
}

/// The page a note is written on. `theme::view_bg` is the one place that literal is written down,
/// for the widgets that cannot read GTK's CSS variables; this is the second such reader.
pub fn page(dark: bool) -> gdk::RGBA {
    gdk::RGBA::parse(crate::theme::view_bg(dark)).unwrap_or(gdk::RGBA::WHITE)
}

/// The accent that text on the page is written in: the standalone one, not `accent_color_rgba`.
/// That one is the brand colour a button is filled with, and it is the same in both halves of the
/// theme. libadwaita darkens it for a light page and lightens it for a dark one before anybody
/// writes text in it, which is what `to_standalone_rgba` hands back — the colour the platform's
/// own links are written in. The link tags, a CSV's columns and the git history's lanes all take it.
fn text_accent() -> gdk::RGBA {
    let style = adw::StyleManager::default();
    style.accent_color().to_standalone_rgba(style.is_dark())
}

/// Apply the standalone accent and the foreground-derived dim colours. Call once after the view is
/// realised and again on every `notify::accent-color` / `notify::dark`.
pub fn restyle(buffer: &sourceview5::Buffer, view: &sourceview5::View) {
    let table = buffer.tag_table();
    let style = adw::StyleManager::default();
    let accent = text_accent();
    let fg = view.color();
    let page = page(style.is_dark());
    let set = |name: &str, f: &dyn Fn(&gtk::TextTag)| {
        if let Some(t) = table.lookup(name) {
            f(&t);
        }
    };
    for name in ["link", "wikilink", "tag", "image"] {
        set(name, &|t| t.set_foreground_rgba(Some(&accent)));
    }
    for name in ["marker", "frontmatter", "listmarker"] {
        set(name, &|t| t.set_foreground_rgba(Some(&dim(fg, page, 0.4))));
    }
    for name in ["quote", "taskdone"] {
        set(name, &|t| t.set_foreground_rgba(Some(&dim(fg, page, 0.6))));
    }
    // A wash behind a run of code rather than ink on the page: it is meant to be barely there, so
    // the floor — which is about a mark being findable — would turn it into a slab.
    for name in ["code", "codeblock"] {
        set(name, &|t| t.set_background_rgba(Some(&theme::at(fg, 0.07))));
    }
}

/// Re-analyse the whole buffer, bring our tags in line with it, and hand the analysis back — with
/// the byte to character table it was tagged through, so a caller that also wants the links in
/// the buffer's own coordinates neither parses the note twice nor rebuilds the table. The
/// analysis needs the text as one contiguous copy, so it stays on the main thread.
///
/// Only what differs is touched ([`sync_tag`]): removing every tag over the whole note and
/// applying it back, which this did until 2026-10, had GTK lay the whole note out again on every
/// pass. Each tag's ranges are read back off the buffer itself rather than kept from the last
/// pass, so an edit, a reload, an undo or [`apply_line`] in between leave nothing to keep in
/// step, and a theme change, which restyles the tags themselves, needs no pass at all.
pub fn apply(buffer: &sourceview5::Buffer) -> (markdown::Analysis, Offsets) {
    let (start, end) = buffer.bounds();
    let text = buffer.text(&start, &end, true);
    let analysis = markdown::analyze(&text);
    let offsets = Offsets::new(&text);
    let mut wanted: HashMap<&str, Vec<Range<i32>>> = HashMap::new();
    for span in &analysis.spans {
        let range = offsets.char_of(span.range.start)..offsets.char_of(span.range.end);
        for name in tags_of(&text, span) {
            wanted.entry(name).or_default().push(range.clone());
        }
    }
    let table = buffer.tag_table();
    let (names, tags): (Vec<&str>, Vec<gtk::TextTag>) = TAG_NAMES
        .iter()
        .filter_map(|&name| Some((name, table.lookup(name)?)))
        .unzip();
    let have = runs_of(buffer, &tags);
    for ((name, tag), have) in names.into_iter().zip(&tags).zip(have) {
        sync_runs(buffer, tag, &have, wanted.remove(name).unwrap_or_default());
    }
    (analysis, offsets)
}

/// Put `tag` over exactly the characters `want` covers, removing and applying it only where the
/// buffer has it otherwise: each apply or remove has GTK lay its whole range out again, whether
/// the tag was there or not, for any tag that can change a line's size — a font, a margin, an
/// underline.
pub fn sync_tag(buffer: &sourceview5::Buffer, tag: &gtk::TextTag, want: Vec<Range<i32>>) {
    sync_runs(buffer, tag, &runs(buffer, tag), want);
}

/// [`sync_tag`], told where `tag` lies now.
fn sync_runs(
    buffer: &sourceview5::Buffer,
    tag: &gtk::TextTag,
    have: &[Range<i32>],
    want: Vec<Range<i32>>,
) {
    let want = merged(want);
    let at = |range: &Range<i32>| {
        (
            buffer.iter_at_offset(range.start),
            buffer.iter_at_offset(range.end),
        )
    };
    for range in minus(have, &want) {
        let (s, e) = at(&range);
        buffer.remove_tag(tag, &s, &e);
    }
    for range in minus(&want, have) {
        let (s, e) = at(&range);
        buffer.apply_tag(tag, &s, &e);
    }
}

/// The parts of `a` that `b` does not cover. Both sorted by start and neither overlapping itself.
fn minus(a: &[Range<i32>], b: &[Range<i32>]) -> Vec<Range<i32>> {
    let (mut out, mut first) = (Vec::new(), 0);
    for r in a {
        // What ends before `r` starts ends before every later range of `a` starts too.
        while first < b.len() && b[first].end <= r.start {
            first += 1;
        }
        let mut from = r.start;
        for cut in b[first..].iter().take_while(|cut| cut.start < r.end) {
            if cut.start > from {
                out.push(from..cut.start);
            }
            from = from.max(cut.end);
        }
        if from < r.end {
            out.push(from..r.end);
        }
    }
    out
}

/// Where `tag` lies in `buffer`, as character ranges in order.
pub fn runs(buffer: &sourceview5::Buffer, tag: &gtk::TextTag) -> Vec<Range<i32>> {
    let (mut out, mut at) = (Vec::new(), buffer.start_iter());
    while at.starts_tag(Some(tag)) || at.forward_to_tag_toggle(Some(tag)) {
        let start = at.offset();
        at.forward_to_tag_toggle(Some(tag));
        out.push(start..at.offset());
    }
    out
}

/// [`runs`] for each of `tags`, read in one walk over every toggle in the buffer. A walk per tag
/// goes over the text again for each, skipping only stretches the tag never touches, which in a
/// note dense with markup is little of it.
fn runs_of(buffer: &sourceview5::Buffer, tags: &[gtk::TextTag]) -> Vec<Vec<Range<i32>>> {
    let index = |tag: &gtk::TextTag| tags.iter().position(|t| t == tag);
    let mut out = vec![Vec::new(); tags.len()];
    let mut open = vec![None; tags.len()];
    let mut at = buffer.start_iter();
    loop {
        let offset = at.offset();
        for tag in at.toggled_tags(false) {
            if let Some(i) = index(&tag)
                && let Some(start) = open[i].take()
            {
                out[i].push(start..offset);
            }
        }
        for tag in at.toggled_tags(true) {
            if let Some(i) = index(&tag) {
                open[i] = Some(offset);
            }
        }
        if !at.forward_to_tag_toggle(None::<&gtk::TextTag>) {
            break;
        }
    }
    // What runs to the end of the text, where the walk has stopped.
    for (runs, start) in out.iter_mut().zip(open) {
        if let Some(start) = start {
            runs.push(start..at.offset());
        }
    }
    out
}

/// `ranges` sorted, with the ones that overlap or touch joined and the empty ones gone: the
/// shape [`runs`] reads a tag back in.
fn merged(mut ranges: Vec<Range<i32>>) -> Vec<Range<i32>> {
    ranges.sort_by_key(|r| r.start);
    let mut out: Vec<Range<i32>> = Vec::with_capacity(ranges.len());
    for r in ranges.into_iter().filter(|r| r.start < r.end) {
        match out.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

/// The tags whose ranges in `buffer` differ from what a fresh pass over the same text gives:
/// every span tagged one by one on a buffer that had none, which is what [`apply`] did before it
/// diffed. For `ACCENT_BENCH_STYLE`, after a run of edits.
#[cfg(feature = "bench")]
pub fn mismatches(buffer: &sourceview5::Buffer) -> Vec<&'static str> {
    let fresh = sourceview5::Buffer::new(None);
    install_tags(&fresh);
    let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
    fresh.set_text(&text);
    let offsets = Offsets::new(&text);
    for span in &markdown::analyze(&text).spans {
        tag_span(&fresh, &offsets, &text, span);
    }
    let lookup = |buffer: &sourceview5::Buffer, name: &str| {
        let tag = buffer.tag_table().lookup(name).expect("an installed tag");
        merged(runs(buffer, &tag))
    };
    TAG_NAMES
        .iter()
        .copied()
        .filter(|name| lookup(buffer, name) != lookup(&fresh, name))
        .collect()
}

/// The tags a span is given: its style's, and where the span is an ATX heading the hanging indent
/// that pulls its `#` markers out into the gutter. [`apply_line`] hands it spans clipped to a
/// single line: clipping can only move a start past the `#` markers, and a line that has not got
/// them has nothing to hang.
fn tags_of(text: &str, span: &Span) -> impl Iterator<Item = &'static str> {
    let hang = match span.style {
        Style::Heading(level) if is_atx(text, span.range.start) => {
            Some(HANG_TAGS[usize::from(level.clamp(1, 6)) - 1])
        }
        _ => None,
    };
    std::iter::once(tag_name(span.style)).chain(hang)
}

/// Tag one span with [`tags_of`] it, over whatever is there already.
fn tag_span(buffer: &sourceview5::Buffer, offsets: &Offsets, text: &str, span: &Span) {
    let s = buffer.iter_at_offset(offsets.char_of(span.range.start));
    let e = buffer.iter_at_offset(offsets.char_of(span.range.end));
    for name in tags_of(text, span) {
        buffer.apply_tag_by_name(name, &s, &e);
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
    for name in TAG_NAMES {
        buffer.remove_tag_by_name(name, &s, &e);
    }
    for span in clipped(&analysis.spans, &bytes) {
        tag_span(buffer, &offsets, &text, &span);
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

/// Tag every cell of `buffer` with its column's tag, replacing whatever was there: a companion's
/// whole text, which is set at once. The editor keeps a [`Csv`] instead.
pub fn apply_csv(buffer: &sourceview5::Buffer) {
    Csv::default().apply(buffer);
}

/// A cell as a pass tags it: its characters and its column.
type Tagged = (i32, i32, usize);

/// A buffer's column colouring, kept from one pass to the next so that a pass after an edit
/// re-tags only the cells the edit can have moved: tagging all of them took 1–2.5 s a pass at
/// 120,000 rows (2026-10-08). A file that ends without a row terminator still has its last cell
/// tagged, and an empty buffer yields no cells at all.
#[derive(Default)]
pub struct Csv {
    /// The cells the last pass left tagged and the length of the text they are in, `None` before
    /// the first pass.
    tagged: RefCell<Option<(Vec<Tagged>, i32)>>,
    /// How many characters lie before the first edit since that pass, and after the last one.
    edited: Cell<Option<(i32, i32)>>,
}

impl Csv {
    /// Colouring for `buffer`, its edits counted from now on, ahead of each landing.
    pub fn tracking(buffer: &sourceview5::Buffer) -> Rc<Csv> {
        let csv = Rc::new(Csv::default());
        let c = csv.clone();
        buffer.connect_insert_text(move |buffer, at, _| {
            c.edit(at.offset(), buffer.char_count() - at.offset());
        });
        let c = csv.clone();
        buffer.connect_delete_range(move |buffer, from, to| {
            c.edit(from.offset(), buffer.char_count() - to.offset());
        });
        csv
    }

    fn edit(&self, before: i32, after: i32) {
        let (b, a) = self.edited.get().unwrap_or((before, after));
        self.edited.set(Some((b.min(before), a.min(after))));
    }

    /// Tag every cell with its column's tag: all of them on the first pass, and after that the
    /// ones [`kept`] cannot vouch for, with what lies between them cleared first.
    pub fn apply(&self, buffer: &sourceview5::Buffer) {
        let edited = self.edited.take();
        let mut tagged = self.tagged.borrow_mut();
        if tagged.is_some() && edited.is_none() {
            return;
        }
        let (start, end) = buffer.bounds();
        let text = buffer.text(&start, &end, true);
        let offsets = Offsets::new(&text);
        let cells: Vec<Tagged> = csv::columns(&text)
            .into_iter()
            .map(|cell| {
                let at = |byte| offsets.char_of(byte);
                (at(cell.range.start), at(cell.range.end), cell.column)
            })
            .collect();
        let total = end.offset();
        let (head, tail) = match (tagged.take(), edited) {
            (Some((old, was)), Some((before, after))) => {
                kept(&old, &cells, total - was, before, total - after)
            }
            _ => (0, 0),
        };
        let from = head.checked_sub(1).map_or(0, |i| cells[i].1);
        let to = cells.get(cells.len() - tail).map_or(total, |cell| cell.0);
        let iter = |offset| buffer.iter_at_offset(offset);
        for name in CSV_TAG_NAMES {
            buffer.remove_tag_by_name(name, &iter(from), &iter(to));
        }
        for &(s, e, column) in &cells[head..cells.len() - tail] {
            buffer.apply_tag_by_name(CSV_TAG_NAMES[column % CSV_COLUMNS], &iter(s), &iter(e));
        }
        *tagged = Some((cells, total));
    }
}

/// How many of `cells` at the start and at the end still carry the tags a pass gave them as
/// `old`, in a text `shift` characters shorter: those wholly before `before`, where the edits
/// since began, or wholly after `after`, where they ended, that parse as they did. Tags move with
/// the text around them, but text typed at the end of a cell takes its tag, so a cell touching an
/// edit is not one of them.
fn kept(old: &[Tagged], cells: &[Tagged], shift: i32, before: i32, after: i32) -> (usize, usize) {
    let head = (old.iter().zip(cells))
        .take_while(|(was, cell)| was == cell && cell.1 < before)
        .count();
    let tail = (old[head..].iter().rev().zip(cells[head..].iter().rev()))
        .take_while(|(was, cell)| (was.0 + shift, was.1 + shift, was.2) == **cell && cell.0 > after)
        .count();
    (head, tail)
}

/// Give the column tags their colours, derived from the current accent. Call it where [`restyle`]
/// is called: the palette follows the system accent and the columns have no other colour source.
pub fn restyle_csv(buffer: &sourceview5::Buffer) {
    let table = buffer.tag_table();
    let accent = text_accent();
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
    let accent = text_accent();
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
    fn a_csv_pass_re_tags_only_the_cells_an_edit_reached() {
        let cells = |text: &str| -> Vec<Tagged> {
            (csv::columns(text).into_iter())
                .map(|c| (c.range.start as i32, c.range.end as i32, c.column))
                .collect()
        };
        let old = cells("a,b\nc,d\ne,f\n");
        // A "d" typed after "d": that cell and nothing else.
        let new = cells("a,b\nc,dd\ne,f\n");
        assert_eq!(kept(&old, &new, 1, 7, 8), (3, 2));
        // "x," typed before "d": the rest of its row moves a column on, the next row does not.
        let new = cells("a,b\nc,x,d\ne,f\n");
        assert_eq!(kept(&old, &new, 2, 6, 8), (3, 2));
        // An opening quote runs on to the end: nothing after it parses as before.
        let new = cells("a,b\n\"c,d\ne,f\n");
        assert_eq!(kept(&old, &new, 1, 4, 5), (2, 0));
        // Text typed at the end of "b" takes its tag, so "b" goes again though it parses the same.
        let new = cells("a,b\n\nc,d\ne,f\n");
        assert_eq!(kept(&old, &new, 1, 3, 4).0, 1);
    }

    #[test]
    fn a_dim_colour_the_page_has_room_for_keeps_its_alpha() {
        // Adwaita light: near-black ink on white paper, 12.6:1 before it is dimmed at all.
        let ink = gdk::RGBA::new(0.0, 0.0, 6.0 / 255.0, 0.8);
        let page = gdk::RGBA::WHITE;
        assert_eq!(dim(ink, page, 0.4).alpha(), 0.4);
        assert!(reads_at(dim(ink, page, 0.4), page) >= DIM_FLOOR);
    }

    #[test]
    fn a_dim_colour_the_page_swallows_is_given_more_ink() {
        // Solarized light: base00 on base3, 4.1:1 to start with, so 40 % of it is 1.6:1.
        let ink = gdk::RGBA::parse("#657b83").unwrap();
        let page = gdk::RGBA::parse("#fdf6e3").unwrap();
        assert!(reads_at(theme::at(ink, 0.4), page) < DIM_FLOOR);
        let lifted = lift(ink, page, 0.4, DIM_FLOOR);
        assert!(lifted.alpha() > 0.4, "alpha {}", lifted.alpha());
        assert!((reads_at(lifted, page) - DIM_FLOOR).abs() < 0.01);
    }

    #[test]
    fn solarized_keeps_a_quote_a_step_above_a_marker() {
        // Base00 on base3 and base0 on base03: the floor lifts the light marker alone.
        for (ink, page, marker) in [("#657b83", "#fdf6e3", 1.80), ("#839496", "#002b36", 1.86)] {
            let (ink, page) = (
                gdk::RGBA::parse(ink).unwrap(),
                gdk::RGBA::parse(page).unwrap(),
            );
            let dimmed = lift(ink, page, 0.4, SOLARIZED_DIM_FLOOR);
            let quote = lift(ink, page, 0.6, SOLARIZED_DIM_FLOOR);
            assert!((reads_at(dimmed, page) - marker).abs() < 0.01);
            assert_eq!(quote.alpha(), 0.6);
            assert!(reads_at(quote, page) > 1.15 * reads_at(dimmed, page));
        }
    }

    #[test]
    fn a_page_with_no_room_left_gets_all_the_ink() {
        // An ink the floor is out of reach of: the answer is the ink, not a colour past it.
        let ink = gdk::RGBA::parse("#888888").unwrap();
        let page = gdk::RGBA::parse("#777777").unwrap();
        assert_eq!(dim(ink, page, 0.4).alpha(), 1.0);
    }

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

    /// What [`apply`] removes and applies: the ranges one side has and the other has not, with
    /// the wanted ones joined first the way GTK reads a tag back.
    #[test]
    fn only_the_difference_is_touched() {
        let want = merged(vec![10..20, 0..5, 15..25, 5..6, 30..30]);
        assert_eq!(want, vec![0..6, 10..25]);
        let have = vec![0..6, 12..14, 22..40];
        assert_eq!(minus(&have, &want), vec![25..40]);
        assert_eq!(minus(&want, &have), vec![10..12, 14..22]);
        assert!(minus(&want, &want).is_empty());
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
