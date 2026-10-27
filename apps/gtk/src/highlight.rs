//! Editor styling: core `Span`s and CSV `Cell`s (byte ranges) -> `gtk::TextTag`s on the buffer.

use crate::theme;
use accent_core::csv;
use accent_core::markdown::{self, Span, Style};
use accent_core::note::Syntax;
use gtk::prelude::*;
use gtk::{gdk, pango};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

/// Translate analysis byte offsets to TextBuffer character offsets.
///
/// Non-ASCII text uses one `u32` per byte for direct lookups, trading 4 MB per 1 MB note
/// for simple conversion once per debounced analysis.
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
        Style::Underline => "underline",
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

/// Shared heading tag names, h1 first, so heading detection cannot drift from styling.
pub const HEADING_TAGS: [&str; 6] = ["h1", "h2", "h3", "h4", "h5", "h6"];
/// The paragraph tags [`hang`] gives each heading level's margins, `hang1` first.
const HANG_TAGS: [&str; 6] = ["hang1", "hang2", "hang3", "hang4", "hang5", "hang6"];
/// The fenced-block tag, for the same reason.
pub const CODEBLOCK: &str = "codeblock";
/// The tag libspelling passes over: GtkSourceView's name for its `no-spell-check` class, which a
/// note, having no language, gets from the styling pass instead ([`unchecked`]).
const NO_SPELL: &str = "gtksourceview:context-classes:no-spell-check";

/// Heading scales indexed by level minus one, shared by tags and [`hang`] measurements.
/// Levels five and six retain body scale.
pub(crate) const HEADING_SCALES: [f64; 6] = [1.6, 1.4, 1.2, 1.1, 1.0, 1.0];

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
    "underline",
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
    NO_SPELL,
];

/// Install theme-independent tags idempotently after wrap::install so heading hangs win priority.
/// [`restyle`] supplies theme colours.
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
    tag("underline").set_underline(pango::Underline::Single);
    let mono = crate::platform::monospace_family();
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
    tag(NO_SPELL);
}

/// Minimum contrast for dim markup against the page.
///
/// Fixed alphas become illegible on lower-contrast palettes such as Solarized. This floor
/// preserves Adwaita's dim-marker contrast across themes without raising punctuation to
/// the 4.5:1 target for readable text and making it compete with prose.
const DIM_FLOOR: f32 = 2.8;

/// Solarized-specific dim contrast floor preserving a distinction between markers and quotes.
/// The normal floor lifts both equally; 1.8:1 scales Adwaita marker contrast to Solarized
/// light prose, while dark retains original marker and quote alpha.
const SOLARIZED_DIM_FLOOR: f32 = 1.8;

/// WCAG 2.1 contrast of `colour` composited over `page`, matching the visible mix rather than raw RGB.
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

/// Raise `ink` alpha minimally to clear `floor` against `page`, or use opaque ink if unattainable.
/// Contrast increases monotonically with alpha, allowing bisection.
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

/// Resolve standalone text accent rather than the fixed button brand colour.
/// Libadwaita adjusts standalone accent for light/dark readability; links, CSV and Git lanes share it.
pub(crate) fn text_accent() -> gdk::RGBA {
    let style = adw::StyleManager::default();
    style.accent_color().to_standalone_rgba(style.is_dark())
}

/// The tags [`restyle`] writes in the accent, and the two sets it dims: what the minimap draws in
/// its accent and in its dimmer tone.
pub const ACCENT_TAGS: [&str; 4] = ["link", "wikilink", "tag", "image"];
pub const MARKUP_TAGS: [&str; 3] = ["marker", "frontmatter", "listmarker"];
pub const FADED_TAGS: [&str; 2] = ["quote", "taskdone"];

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
    for name in ACCENT_TAGS {
        set(name, &|t| t.set_foreground_rgba(Some(&accent)));
    }
    for name in MARKUP_TAGS {
        set(name, &|t| t.set_foreground_rgba(Some(&dim(fg, page, 0.4))));
    }
    for name in FADED_TAGS {
        set(name, &|t| t.set_foreground_rgba(Some(&dim(fg, page, 0.6))));
    }
    // A wash behind a run of code rather than ink on the page: it is meant to be barely there, so
    // the floor — which is about a mark being findable — would turn it into a slab.
    for name in ["code", "codeblock"] {
        set(name, &|t| t.set_background_rgba(Some(&theme::at(fg, 0.07))));
    }
    crate::codeblocks::restyle(buffer);
}

/// Analyse the buffer, sync tags and return analysis plus its byte-to-character table.
///
/// Shared results avoid duplicate parsing and coordinate conversion for link consumers.
/// Only changed ranges are tagged to avoid full relayout. Existing buffer tags remain the
/// source of truth after edits, reloads and Undo; theme changes restyle tags without a pass.
pub fn apply(buffer: &sourceview5::Buffer, syntax: Syntax) -> (markdown::Analysis, Offsets) {
    let (start, end) = buffer.bounds();
    let text = buffer.text(&start, &end, true);
    let analysis = syntax.analyze(&text);
    let offsets = Offsets::new(&text);
    let mut wanted: HashMap<&str, Vec<Range<i32>>> = HashMap::new();
    for span in &analysis.spans {
        let range = offsets.char_of(span.range.start)..offsets.char_of(span.range.end);
        for name in tags_of(&text, span, syntax) {
            wanted.entry(name).or_default().push(range.clone());
        }
        if let Some(r) = unchecked(&text, span, syntax) {
            let range = offsets.char_of(r.start)..offsets.char_of(r.end);
            wanted.entry(NO_SPELL).or_default().push(range);
        }
    }
    for r in addresses(&text, syntax) {
        let range = offsets.char_of(r.start)..offsets.char_of(r.end);
        wanted.entry(NO_SPELL).or_default().push(range);
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
    let blocks = match analysis.spans.iter().any(|s| s.style == Style::CodeBlock) {
        true => syntax.code_blocks(&text),
        false => Vec::new(),
    };
    crate::codeblocks::apply(buffer, syntax, &text, &offsets, &blocks);
    (analysis, offsets)
}

/// Apply `tag` only where current ranges differ from `want`.
/// Even redundant size-affecting tag changes trigger GTK relayout, so exact range differences matter.
pub fn sync_tag(buffer: &sourceview5::Buffer, tag: &gtk::TextTag, want: Vec<Range<i32>>) {
    sync_runs(buffer, tag, &runs(buffer, tag), want);
}

/// [`sync_tag`] for several tags, where each lies now read in one walk over the buffer.
pub fn sync_tags(buffer: &sourceview5::Buffer, wanted: Vec<(gtk::TextTag, Vec<Range<i32>>)>) {
    let tags: Vec<gtk::TextTag> = wanted.iter().map(|(tag, _)| tag.clone()).collect();
    for ((tag, want), have) in wanted.into_iter().zip(runs_of(buffer, &tags)) {
        sync_runs(buffer, &tag, &have, want);
    }
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

/// Read [`runs`] for every tag in one toggle walk, avoiding repeated scans of markup-dense text.
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

/// Find styling differences from a fresh full pass for ACCENT_BENCH_STYLE after edits.
#[cfg(feature = "bench")]
pub fn mismatches(buffer: &sourceview5::Buffer) -> Vec<&'static str> {
    let fresh = sourceview5::Buffer::new(None);
    install_tags(&fresh);
    let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
    fresh.set_text(&text);
    let offsets = Offsets::new(&text);
    for span in &markdown::analyze(&text).spans {
        tag_span(&fresh, &offsets, &text, span, Syntax::Markdown);
    }
    for r in addresses(&text, Syntax::Markdown) {
        tag_bytes(&fresh, &offsets, NO_SPELL, r);
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

/// Return span style tags and ATX hanging tags when visible markers remain.
/// [`apply_line`] clipping may exclude markers; clipped body text must not receive heading hang.
fn tags_of(text: &str, span: &Span, syntax: Syntax) -> impl Iterator<Item = &'static str> {
    let hang = match span.style {
        Style::Heading(level) if is_atx(text, span.range.start, heading_mark(syntax)) => {
            Some(HANG_TAGS[usize::from(level.clamp(1, 6)) - 1])
        }
        _ => None,
    };
    std::iter::once(tag_name(span.style)).chain(hang)
}

/// Tag one span with [`tags_of`] it, over whatever is there already.
fn tag_span(
    buffer: &sourceview5::Buffer,
    offsets: &Offsets,
    text: &str,
    span: &Span,
    syntax: Syntax,
) {
    let s = buffer.iter_at_offset(offsets.char_of(span.range.start));
    let e = buffer.iter_at_offset(offsets.char_of(span.range.end));
    for name in tags_of(text, span, syntax) {
        buffer.apply_tag_by_name(name, &s, &e);
    }
    if let Some(r) = unchecked(text, span, syntax) {
        tag_bytes(buffer, offsets, NO_SPELL, r);
    }
}

/// Tag the bytes `range` of the text with the tag called `name`.
fn tag_bytes(buffer: &sourceview5::Buffer, offsets: &Offsets, name: &str, range: Range<usize>) {
    let s = buffer.iter_at_offset(offsets.char_of(range.start));
    let e = buffer.iter_at_offset(offsets.char_of(range.end));
    buffer.apply_tag_by_name(name, &s, &e);
}

/// What of `span` the spell checker passes over: code, math, markup and tags whole, and of a link
/// its target, the text it shows being prose.
fn unchecked(text: &str, span: &Span, syntax: Syntax) -> Option<Range<usize>> {
    let r = span.range.clone();
    let within = match span.style {
        Style::CodeInline
        | Style::CodeBlock
        | Style::Math
        | Style::Html
        | Style::Frontmatter
        | Style::Tag => 0..r.len(),
        Style::Link | Style::WikiLink | Style::Image => target(&text[r.clone()], syntax),
        _ => return None,
    };
    Some(r.start + within.start..r.start + within.end)
}

/// The bare addresses in a markdown note's `text` ([`markdown::bare_urls`]), which the spell
/// checker passes over as it does a link's target: pulldown-cmark reads no link in them, where
/// org reads one, whose span [`unchecked`] covers.
fn addresses(text: &str, syntax: Syntax) -> impl Iterator<Item = Range<usize>> + '_ {
    (syntax == Syntax::Markdown)
        .then(|| markdown::bare_urls(text))
        .into_iter()
        .flatten()
}

/// Where the target is in a link written as `link`: all of `[[target]]`, `<address>` or a bare
/// address, the head of `[[target|alias]]` (org's `[[target][description]]`), and the tail of
/// `[text](target)` and `[text][label]`.
fn target(link: &str, syntax: Syntax) -> Range<usize> {
    let bare = link.strip_prefix('!').unwrap_or(link);
    let alias = match syntax {
        Syntax::Markdown => "|",
        Syntax::Org => "][",
    };
    if bare.starts_with("[[") {
        return 0..link.find(alias).map_or(link.len(), |at| at + alias.len());
    }
    if bare.starts_with('[') {
        let at = link.find("](").or_else(|| link.find("]["));
        return at.unwrap_or(link.len())..link.len();
    }
    0..link.len()
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

/// Clip overlapping spans to `range` in full-pass order.
/// Enclosing fence/frontmatter spans retain the full clipped line, preserving document context.
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

/// Style one line from a whole-document analysis while long notes await a debounced full pass.
///
/// Parsing is cheaper than tag churn. Whole-document context keeps fence and frontmatter
/// styling correct while only the edited line's tags change.
pub fn apply_line(buffer: &sourceview5::Buffer, line: i32, syntax: Syntax) {
    let (start, end) = buffer.bounds();
    let text = buffer.text(&start, &end, true);
    let Some(bytes) = usize::try_from(line)
        .ok()
        .and_then(|n| line_bytes(&text, n))
    else {
        return;
    };
    let analysis = syntax.analyze(&text);
    let offsets = Offsets::new(&text);
    let s = buffer.iter_at_offset(offsets.char_of(bytes.start));
    let e = buffer.iter_at_offset(offsets.char_of(bytes.end));
    for name in TAG_NAMES {
        buffer.remove_tag_by_name(name, &s, &e);
    }
    for span in clipped(&analysis.spans, &bytes) {
        tag_span(buffer, &offsets, &text, &span, syntax);
    }
    for r in addresses(&text[bytes.clone()], syntax) {
        tag_bytes(
            buffer,
            &offsets,
            NO_SPELL,
            bytes.start + r.start..bytes.start + r.end,
        );
    }
}

/// What a heading opens with in `syntax`: markdown's `#`, org's `*`.
fn heading_mark(syntax: Syntax) -> char {
    match syntax {
        Syntax::Markdown => '#',
        Syntax::Org => '*',
    }
}

/// Whether an ATX heading at byte `start` has `mark` markers followed by a space.
/// Setext headings have no markers. Waiting for the space avoids shifting a typed bare #
/// that may become a tag.
fn is_atx(text: &str, start: usize, mark: char) -> bool {
    let rest = &text[start..];
    let after = rest.trim_start_matches(mark);
    after.len() < rest.len() && after.starts_with([' ', '\t'])
}

/// Hang ATX heading markers into the left gutter, aligning heading text with prose.
///
/// Measure markers in the current font, heading scale and zoom. Negative Pango indent and
/// a matching left margin align wrapped rows with the body column. The fixed 48 px gutter
/// can clip level-five and level-six markers, shifting their text slightly right.
pub fn hang(buffer: &sourceview5::Buffer, view: &sourceview5::View, syntax: Syntax) {
    let table = buffer.tag_table();
    let gutter = view.left_margin();
    for (level, scale) in HEADING_SCALES.iter().enumerate() {
        let Some(tag) = table.lookup(&format!("hang{}", level + 1)) else {
            continue;
        };
        // The layout inherits the view's font from its pango context, so whatever CSS says about
        // the document font and the zoom is already in it; the two attributes are exactly what
        // the matching `h{n}` tag adds on top of it.
        let marks = heading_mark(syntax).to_string().repeat(level + 1);
        let layout = view.create_pango_layout(Some(&format!("{marks} ")));
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

/// CSV tag names kept separate from [`TAG_NAMES`] so either styling pass cannot clear the other.
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
pub fn apply_csv(buffer: &sourceview5::Buffer, delim: Option<u8>) {
    Csv::default().apply(buffer, delim);
}

/// A cell as a pass tags it: its characters and its column.
type Tagged = (i32, i32, usize);

/// Incremental CSV colouring state, retagging only records affected by edits.
/// Whole-file passes are expensive on large CSVs. Unterminated final records still contribute cells;
/// empty buffers contribute none.
#[derive(Default)]
pub struct Csv {
    /// The records the last pass read, `None` before the first pass.
    rows: RefCell<Option<Rows>>,
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

    /// Tag all cells initially, then edited records until [`Rows::resume`] rejoins unchanged records.
    /// Clear old tags only within the reread records. `delim` is a TSV's tab, `None` a CSV's
    /// sniffed delimiter.
    pub fn apply(&self, buffer: &sourceview5::Buffer, delim: Option<u8>) {
        let mut rows = self.rows.borrow_mut();
        let (before, after) = match (rows.is_some(), self.edited.take()) {
            (true, None) => return,
            (true, Some(edited)) => edited,
            // The first pass reads it all, as an edit over all of it would.
            (false, _) => (0, 0),
        };
        let old = rows.take().unwrap_or_default();
        let total = buffer.char_count();
        let (from, end) = (old.start_before(before), total - after);
        let iter = |offset| buffer.iter_at_offset(offset);
        // To the line after the last edit, where an edit inside a record is back in step; four
        // times as many lines each time the read is not, a quote opened or closed running on.
        let mut lines = 1;
        let (cells, to, delim) = loop {
            let mut until = iter(end);
            until.forward_lines(lines);
            let text = buffer.text(&iter(from), &until, true);
            let read = old.resume(&text, from, end, total, delim);
            if read.1.is_some() || until.is_end() {
                break read;
            }
            lines *= 4;
        };
        let to = to.unwrap_or(total);
        for name in CSV_TAG_NAMES {
            buffer.remove_tag_by_name(name, &iter(from), &iter(to));
        }
        for &(s, e, column) in &cells {
            buffer.apply_tag_by_name(CSV_TAG_NAMES[column % CSV_COLUMNS], &iter(s), &iter(e));
        }
        *rows = Some(old.spliced(&cells, from, to, total, delim));
    }
}

/// Character record starts, delimiter and text length.
/// Each start lies outside quotes, allowing incremental parsing to resume there.
#[derive(Default)]
struct Rows {
    starts: Vec<i32>,
    delim: u8,
    len: i32,
}

impl Rows {
    /// The last record start at or before `offset`: an edit there leaves the text before it, and
    /// so the read up to it, as they were.
    fn start_before(&self, offset: i32) -> i32 {
        let i = self.starts.partition_point(|&s| s <= offset);
        i.checked_sub(1).map_or(0, |i| self.starts[i])
    }

    /// Read edited `text` from record start `from` until an unchanged start at or beyond `end`.
    /// Return cells, the resume start if any, and delimiter: `fixed`, else sniffed again only at
    /// the file's start.
    fn resume(
        &self,
        text: &str,
        from: i32,
        end: i32,
        len: i32,
        fixed: Option<u8>,
    ) -> (Vec<Tagged>, Option<i32>, u8) {
        let offsets = Offsets::new(text);
        let at = |byte| from + offsets.char_of(byte);
        let delim = match from {
            0 => fixed.unwrap_or_else(|| csv::delimiter(text)),
            _ => self.delim,
        };
        let shift = len - self.len;
        let (cells, stop) = csv::columns_until(text, delim, |byte| {
            let start = at(byte);
            delim == self.delim
                && start >= end
                && self.starts.binary_search(&(start - shift)).is_ok()
        });
        let cells = (cells.into_iter())
            .map(|cell| (at(cell.range.start), at(cell.range.end), cell.column))
            .collect();
        (cells, stop.map(at), delim)
    }

    /// These rows with the records from `from` to `to` read again as `cells`, in a text now `len`
    /// characters long and read with `delim`.
    fn spliced(mut self, cells: &[Tagged], from: i32, to: i32, len: i32, delim: u8) -> Rows {
        let shift = len - self.len;
        let first = self.starts.partition_point(|&s| s < from);
        let last = self.starts.partition_point(|&s| s < to - shift);
        for start in &mut self.starts[last..] {
            *start += shift;
        }
        let read = cells.iter().filter(|cell| cell.2 == 0).map(|cell| cell.0);
        self.starts.splice(first..last, read);
        Rows {
            starts: self.starts,
            delim,
            len,
        }
    }
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

/// Git lane colour from the CSV six-hue accent wheel.
/// Lane 0 is the accent and lane 6 repeats it, preserving DESIGN.md's colour rule.
pub fn lane_colour(column: usize) -> gdk::RGBA {
    let accent = text_accent();
    let hsv = gtk::rgb_to_hsv(accent.red(), accent.green(), accent.blue());
    let (h, s, v) = rotate(hsv, column % CSV_COLUMNS);
    let (r, g, b) = gtk::hsv_to_rgb(h, s, v);
    gdk::RGBA::new(r, g, b, accent.alpha())
}

/// Rotate accent hue by `column` sixths, preserving saturation and value for one colour family.
pub(crate) fn rotate(hsv: (f32, f32, f32), column: usize) -> (f32, f32, f32) {
    let (h, s, v) = hsv;
    ((h + column as f32 / CSV_COLUMNS as f32).fract(), s, v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_csv_pass_reads_from_the_edited_record_until_it_is_in_step_again() {
        let old = "a,b\nc,d\ne,f\n";
        let (cells, _, delim) = Rows::default().resume(old, 0, 0, 12, None);
        let rows = Rows::default().spliced(&cells, 0, 12, 12, delim);
        // A quote opened before "c" runs on past the edit: every record after it is read again.
        let new = "a,b\n\"c,d\ne,f\n";
        assert_eq!(rows.start_before(4), 4);
        assert_eq!(
            rows.resume(&new[4..], 4, 5, 13, None),
            (vec![(4, 13, 0)], None, b',')
        );
        // A "d" typed after "d": its record alone, the next one starting where it did.
        let new = "a,b\nc,dd\ne,f\n";
        assert_eq!(rows.start_before(7), 4);
        let read = rows.resume(&new[4..], 4, 8, 13, None);
        assert_eq!(read, (vec![(4, 5, 0), (6, 8, 1)], Some(9), b','));
        assert_eq!(rows.spliced(&read.0, 4, 9, 13, b',').starts, [0, 4, 9]);
    }

    #[test]
    fn a_tsv_is_read_with_tabs_whatever_its_first_line_holds() {
        // Sniffed, the comma ties with the tab and wins.
        let text = "Name, given\tAge\n";
        let (cells, _, delim) = Rows::default().resume(text, 0, 0, 16, Some(b'\t'));
        assert_eq!((cells, delim), (vec![(0, 11, 0), (12, 15, 1)], b'\t'));
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
            Style::Underline,
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

    /// The spell checker passes over code, math, tags and a link's target, never its text.
    #[test]
    fn a_links_text_is_spell_checked_and_its_target_is_not() {
        let unchecked_in = |text: &'static str, syntax: Syntax| -> Vec<&'static str> {
            let mut out: Vec<&str> = syntax
                .analyze(text)
                .spans
                .iter()
                .filter_map(|span| unchecked(text, span, syntax))
                .map(|r| &text[r])
                .collect();
            out.sort();
            out
        };
        let markdown = "[a text](a.md) [[Note|an alias]] [[Other]] ![an alt](i.png) `c` #tag $x$";
        assert_eq!(
            unchecked_in(markdown, Syntax::Markdown),
            [
                "#tag",
                "$x$",
                "[[Note|",
                "[[Other]]",
                "](a.md)",
                "](i.png)",
                "`c`"
            ]
        );
        let org = "* H :tag:\n[[file:a.org][a text]] [[*H]] https://x.org =v=\n";
        assert_eq!(
            unchecked_in(org, Syntax::Org),
            ["[[*H]]", "[[file:a.org][", "https://x.org", "tag", "v"]
        );
    }

    /// A markdown note's bare addresses pass as a click finds them; org's are links already.
    #[test]
    fn a_bare_address_is_not_spell_checked() {
        let text = "see https://e.org/wrdz, (mailto:a@b.org). not ftp://x or https://\n";
        let found: Vec<&str> = addresses(text, Syntax::Markdown)
            .map(|r| &text[r])
            .collect();
        assert_eq!(found, ["https://e.org/wrdz", "mailto:a@b.org"]);
        assert_eq!(addresses(text, Syntax::Org).count(), 0);
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
        assert!(is_atx(text, starts[0], '#'));
        assert!(!is_atx(text, starts[1], '#'));
        // A heading is only a heading once the space is there; until then the line may still turn
        // into a tag, and a marker that hangs and un-hangs per keystroke is worse than one that waits.
        assert!(!is_atx("#", 0, '#'));
        assert!(!is_atx("#tag", 0, '#'));
        assert!(is_atx("### Deep", 0, '#'));
        // Org's stars hang as a note's `#` do.
        assert!(is_atx("** Deep", 0, heading_mark(Syntax::Org)));
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

    /// Verify six distinct rotating hues preserve accent saturation/value without requiring GTK initialisation.
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
