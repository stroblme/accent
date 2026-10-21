//! Paint language-server diagnostics according to their severity.
//!
//! Errors and warnings get wavy underlines, gutter icons and inline messages, and contribute
//! to the status count. Information and hints get dim underlines with messages in the hover,
//! since conditions such as dangling wikilinks are common while editing.
//!
//! Fixed hues mixed with the theme foreground keep diagnostics readable in light and dark
//! themes; colour literals remain in `theme.rs`.

use std::cell::{Cell, Ref, RefCell};
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use crate::debug::breakpoints;
use crate::{highlight, lang};
use accent_api::{Diagnostic, Pos, Severity};
use gtk::prelude::*;
use gtk::{gdk, pango};
use sourceview5::AnnotationStyle;
use sourceview5::prelude::*;

/// One tag per severity, so a re-render can lift exactly what it painted.
const ERROR: &str = "diag-error";
const WARNING: &str = "diag-warning";
const INFO: &str = "diag-info";
const HINT: &str = "diag-hint";
const TAGS: [&str; 4] = [ERROR, WARNING, INFO, HINT];

/// Gutter mark categories. Only the two severities that carry an icon have one.
pub const MARK_ERROR: &str = "error";
pub const MARK_WARNING: &str = "warning";

/// Opacity for information and hint underlines, with hints dimmest because they are not yet problems.
const INFO_ALPHA: f32 = 0.4;
const HINT_ALPHA: f32 = 0.25;

fn tag_of(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => ERROR,
        Severity::Warning => WARNING,
        Severity::Info => INFO,
        Severity::Hint => HINT,
    }
}

/// Install severity tags with fixed underline shapes; [`restyle`] supplies colours.
/// Shapes distinguish errors from hints independently of colour.
pub fn install_tags(buffer: &sourceview5::Buffer) {
    let table = buffer.tag_table();
    for name in TAGS {
        let underline = match name {
            ERROR | WARNING => pango::Underline::Error,
            _ => pango::Underline::Single,
        };
        let tag = gtk::TextTag::builder()
            .name(name)
            .underline(underline)
            .build();
        table.add(&tag);
    }
}

/// Re-derive the underline colours from the resolved theme foreground. Called from `Tab::restyle`,
/// which runs on the first map and again on every theme change.
pub fn restyle(buffer: &sourceview5::Buffer, view: &sourceview5::View) {
    let fg = view.color();
    let table = buffer.tag_table();
    let set = |name: &str, colour: gdk::RGBA| {
        if let Some(tag) = table.lookup(name) {
            tag.set_underline_rgba(Some(&colour));
        }
    };
    set(ERROR, crate::diff::tint(crate::diff::REMOVED_HUE, fg, 1.0));
    set(
        WARNING,
        crate::diff::tint(crate::diff::WARNING_HUE, fg, 1.0),
    );
    set(INFO, crate::theme::at(fg, INFO_ALPHA));
    set(HINT, crate::theme::at(fg, HINT_ALPHA));
}

/// The end-of-line messages up, by line: each one's annotation and the whole text it was cut
/// from, which [`refit`] cuts again when the line changes.
pub type Shown = BTreeMap<i32, (sourceview5::Annotation, String)>;

/// Replace diagnostics with `items` and return end-of-line messages.
///
/// Reuse unchanged messages from `kept` to avoid measuring again. [`highlight::sync_tag`]
/// updates only changed underlines, avoiding full relayout on each diagnostic publication.
pub fn render(
    view: &sourceview5::View,
    buffer: &sourceview5::Buffer,
    provider: &sourceview5::AnnotationProvider,
    items: &[Diagnostic],
    kept: Shown,
) -> Shown {
    let (start, end) = buffer.bounds();
    let mut underlined: BTreeMap<&str, Vec<std::ops::Range<i32>>> = BTreeMap::new();
    for category in [MARK_ERROR, MARK_WARNING] {
        buffer.remove_source_marks(&start, &end, Some(category));
    }
    provider.remove_all();

    let mut lines: BTreeMap<i32, (AnnotationStyle, String, usize)> = BTreeMap::new();
    for item in items {
        let (from, mut to) = (
            lang::iter_at(buffer, item.range.start),
            lang::iter_at(buffer, item.range.end),
        );
        // A zero-width range is what a server sends for "here", and a tag over no characters
        // draws nothing. One character is the smallest thing the eye can be pointed at.
        if from == to {
            to.forward_char();
        }
        underlined
            .entry(tag_of(item.severity))
            .or_default()
            .push(from.offset()..to.offset());

        let (category, style) = match item.severity {
            Severity::Error => (MARK_ERROR, AnnotationStyle::Error),
            Severity::Warning => (MARK_WARNING, AnnotationStyle::Warning),
            // Information and hints stay in the text: no gutter icon, no line-end message.
            Severity::Info | Severity::Hint => continue,
        };
        let mut line_start = from;
        line_start.set_line_offset(0);
        // A breakpoint's dot takes the line's cell: GtkSourceView draws the icons of all the marks
        // on a line, one over another, whatever their priorities.
        if !breakpoints::on_line(buffer, line_start.line()) {
            buffer.create_source_mark(None, category, &line_start);
        }
        // A line nothing on screen stands for has no row of its own to write on: every message
        // in a hidden run would be drawn at the one row that does, piling up under it. The
        // gutter icon is left to say there is something in there.
        if out_of_sight(&line_start) {
            continue;
        }
        // One annotation a line, or the messages draw over each other at the line end: the most
        // severe one is shown and the rest are counted, and the hover lists them all.
        match lines.entry(from.line()) {
            Entry::Vacant(slot) => {
                slot.insert((style, item.message.clone(), 0));
            }
            Entry::Occupied(mut slot) => {
                let shown = slot.get_mut();
                if style == AnnotationStyle::Error && shown.0 != AnnotationStyle::Error {
                    (shown.0, shown.1) = (style, item.message.clone());
                }
                shown.2 += 1;
            }
        }
    }
    let table = buffer.tag_table();
    for name in TAGS {
        if let Some(tag) = table.lookup(name) {
            highlight::sync_tag(buffer, &tag, underlined.remove(name).unwrap_or_default());
        }
    }
    let mut shown = Shown::new();
    for (line, (style, message, more)) in lines {
        let text = match more {
            0 => message,
            n => format!("{message} (+{n} more)"),
        };
        let cut = match kept.get(&line) {
            Some((annotation, was)) if *was == text => annotation.description().into(),
            _ => fit(view, line, text.clone()),
        };
        let annotation =
            sourceview5::Annotation::new(Some(&cut), None::<gtk::gio::Icon>, line, style);
        provider.add_annotation(&annotation);
        shown.insert(line, (annotation, text));
    }
    shown
}

/// What a language server said about one view's text, and what of it is painted there.
pub struct Painted {
    view: sourceview5::View,
    buffer: sourceview5::Buffer,
    /// The messages at the line ends.
    provider: sourceview5::AnnotationProvider,
    /// The last publish, moved with the text since: what the hover and the status count read.
    items: RefCell<Vec<Diagnostic>>,
    /// Kept out of the text, the hover and the count keeping them.
    hidden: Cell<bool>,
    /// Last painted line messages with their full source text. Hidden lines are omitted; the
    /// provider cannot supply the original text back.
    shown: RefCell<Shown>,
    /// Line messages to relayout after the ranges moved with an edit ([`Painted::shift`]).
    relay: RefCell<Option<Shown>>,
}

impl Painted {
    /// Paint over `view`, which shows `buffer`, its line-end messages through a provider of
    /// their own.
    pub fn new(view: &sourceview5::View, buffer: &sourceview5::Buffer) -> Painted {
        let provider = sourceview5::AnnotationProvider::new();
        view.annotations().add_provider(&provider);
        Painted {
            view: view.clone(),
            buffer: buffer.clone(),
            provider,
            items: RefCell::default(),
            hidden: Cell::new(false),
            shown: RefCell::default(),
            relay: RefCell::new(None),
        }
    }

    /// Replace them with the provider's full publish. Empty clears them.
    pub fn set(&self, items: Vec<Diagnostic>) {
        *self.items.borrow_mut() = items;
        self.paint();
    }

    /// Take them out of the text, or put them back, keeping what was said.
    pub fn hide(&self, hidden: bool) {
        if self.hidden.replace(hidden) != hidden {
            self.paint();
        }
    }

    pub fn hidden(&self) -> bool {
        self.hidden.get()
    }

    /// What the server said, painted or not.
    pub fn items(&self) -> Ref<'_, Vec<Diagnostic>> {
        self.items.borrow()
    }

    /// The line-end messages up.
    pub fn messages(&self) -> Ref<'_, Shown> {
        self.shown.borrow()
    }

    /// Repaint, or clear while hidden, every message measured again.
    pub fn paint(&self) {
        self.lay(Shown::new());
    }

    /// The gutter marks again after a breakpoint came or went, which takes its line's cell from
    /// them ([`render`]); each message keeps its cut.
    pub fn remark(&self) {
        let kept = self.shown.take();
        self.lay(kept);
    }

    /// [`Painted::paint`], keeping the cut of each message in `kept` still on its line.
    fn lay(&self, kept: Shown) {
        let items = self.items.borrow();
        let painted: &[Diagnostic] = match self.hidden.get() {
            true => &[],
            false => &items,
        };
        *self.shown.borrow_mut() = render(&self.view, &self.buffer, &self.provider, painted, kept);
    }

    /// Move them for an upcoming `from..to` edit ([`Edit`]) so hover and repaint use current
    /// positions before the next publish.
    pub fn shift(&self, from: &gtk::TextIter, to: &gtk::TextIter, text: &str) {
        let edit = Edit::new(lang::pos_of(from), lang::pos_of(to), text);
        if edit.shift(&mut self.items.borrow_mut()) {
            *self.relay.borrow_mut() = Some(edit.reline(self.shown.take()));
        }
    }

    /// The text changed, the caret now on `caret_line`: messages an edit moved to another line
    /// are laid again there, and the caret's line is cut again to the room typing left it.
    pub fn edited(&self, caret_line: i32) {
        // GtkSourceView pins a message to its line number, where the underline and the gutter
        // mark ride along with the text: one the edit moved to another line is laid again there.
        if let Some(kept) = self.relay.take() {
            self.lay(kept);
        }
        refit(
            &self.view,
            &self.provider,
            &mut self.shown.borrow_mut(),
            caret_line,
        );
    }
}

/// Refit `line`'s message to remaining width after edits, replacing its immutable annotation.
fn refit(
    view: &sourceview5::View,
    provider: &sourceview5::AnnotationProvider,
    shown: &mut Shown,
    line: i32,
) {
    let Some((annotation, text)) = shown.get_mut(&line) else {
        return;
    };
    let cut = fit(view, line, text.clone());
    if annotation.description() == cut {
        return;
    }
    provider.remove_annotation(annotation);
    *annotation =
        sourceview5::Annotation::new(Some(&cut), None::<gtk::gio::Icon>, line, annotation.style());
    provider.add_annotation(annotation);
}

/// Each message up as its characters shown beside those its line has room for now. Only
/// `ACCENT_BENCH_DIAG` reads it.
#[cfg(feature = "bench")]
pub fn cuts(view: &sourceview5::View, shown: &Shown) -> Vec<(usize, usize)> {
    let chars = |s: &str| s.chars().count();
    shown
        .iter()
        .map(|(line, (annotation, text))| {
            let fits = fit(view, *line, text.clone());
            (chars(&annotation.description()), chars(&fits))
        })
        .collect()
}

/// Truncate `text` to the remaining column width after `line`.
/// GtkSourceView otherwise draws full-width messages and clips mid-glyph; hover and gutter retain the full message.
fn fit(view: &sourceview5::View, line: i32, text: String) -> String {
    let visible = view.visible_rect();
    // Not laid out yet: the tab lays its messages again once it has a width.
    if visible.width() == 0 {
        return text;
    }
    let Some(mut end) = view.buffer().iter_at_line(line) else {
        return text;
    };
    if !end.ends_line() {
        end.forward_to_line_end();
    }
    let at = view.iter_location(&end);
    let room = visible.x() + visible.width() - (at.x() + at.width()) - 2 * at.height();
    // Measured as GtkSourceView measures it: a layout in the view's own font.
    ellipsize(text, room, |s| {
        view.create_pango_layout(Some(s)).pixel_size().0
    })
}

/// `text` if it is no wider than `room`, as `width` measures it, or its longest start that fits
/// with an ellipsis after it; the ellipsis alone where nothing does, which still says there was
/// something.
fn ellipsize(text: String, room: i32, width: impl Fn(&str) -> i32) -> String {
    if width(&text) <= room {
        return text;
    }
    let cut = |chars: usize| {
        let end = text
            .char_indices()
            .nth(chars)
            .map_or(text.len(), |(i, _)| i);
        format!("{}…", text[..end].trim_end())
    };
    // A longer start is never narrower, so the answer is a binary search away: `over` is known
    // not to fit, and `fits` fits or is the ellipsis alone.
    let (mut fits, mut over) = (0, text.chars().count());
    while over - fits > 1 {
        let mid = (fits + over) / 2;
        match width(&cut(mid)) <= room {
            true => fits = mid,
            false => over = mid,
        }
    }
    cut(fits)
}

/// Whether text starting at `at` is hidden by [`crate::fold::hiding`].
/// Fold headers remain visible and retain their messages.
fn out_of_sight(at: &gtk::TextIter) -> bool {
    !crate::fold::hiding(&at.buffer(), at).is_empty()
}

/// How much of `buffer` the diagnostics are painted over: how many underlined runs it carries and
/// how many gutter marks. Only `ACCENT_BENCH_DIAG` reads it.
#[cfg(feature = "bench")]
pub fn painted(buffer: &sourceview5::Buffer) -> (usize, usize) {
    let table = buffer.tag_table();
    let runs = |tag: gtk::TextTag| {
        let mut at = buffer.start_iter();
        // A tag over the first character toggles where the walk begins rather than where it
        // lands, so that one is counted before the walk.
        let mut runs = usize::from(at.starts_tag(Some(&tag)));
        while at.forward_to_tag_toggle(Some(&tag)) {
            runs += usize::from(at.starts_tag(Some(&tag)));
        }
        runs
    };
    let underlined = TAGS.iter().filter_map(|n| table.lookup(n)).map(runs).sum();
    // Line by line: the binding leaves `forward_iter_to_source_mark` unimplemented, and a mark
    // sits at the start of its line, so a pass over the lines sees every one of them.
    let marks = (0..buffer.line_count())
        .flat_map(|line| {
            [MARK_ERROR, MARK_WARNING].map(|c| buffer.source_marks_at_line(line, Some(c)).len())
        })
        .sum();
    (underlined, marks)
}

/// The lines the error underlines start on and those the error marks are on. Only
/// `ACCENT_BENCH_DIAG` reads it.
#[cfg(feature = "bench")]
pub fn error_lines(buffer: &sourceview5::Buffer) -> (Vec<i32>, Vec<i32>) {
    let Some(tag) = buffer.tag_table().lookup(ERROR) else {
        return Default::default();
    };
    let mut at = buffer.start_iter();
    let mut underlined = Vec::new();
    loop {
        if at.starts_tag(Some(&tag)) {
            underlined.push(at.line());
        }
        if !at.forward_to_tag_toggle(Some(&tag)) {
            break;
        }
    }
    let marks = (0..buffer.line_count())
        .filter(|&line| {
            !buffer
                .source_marks_at_line(line, Some(MARK_ERROR))
                .is_empty()
        })
        .collect();
    (underlined, marks)
}

/// Compare diagnostic positions in reading order using tuples; wire type Pos does not define Ord.
fn key(pos: Pos) -> (u32, u32) {
    (pos.line, pos.character)
}

/// An edit about to replace the text from `from` to `to` with `text`, and where it takes what
/// follows: the rest of the text keeps its place, and what the edit removes closes up at `from`.
pub struct Edit {
    from: Pos,
    to: Pos,
    /// Where the new text will end.
    end: Pos,
}

impl Edit {
    pub fn new(from: Pos, to: Pos, text: &str) -> Edit {
        // GTK also breaks a line at a lone `\r`, which nobody types.
        let end = match text.rsplit_once('\n') {
            Some((before, last)) => Pos {
                line: from.line + before.matches('\n').count() as u32 + 1,
                character: last.chars().count() as u32,
            },
            None => Pos {
                line: from.line,
                character: from.character + text.chars().count() as u32,
            },
        };
        Edit { from, to, end }
    }

    fn moved(&self, p: Pos) -> Pos {
        let Edit { from, to, end } = *self;
        match p {
            p if key(p) < key(from) => p,
            _ if key(p) < key(to) => from,
            p if p.line == to.line => Pos {
                line: end.line,
                character: end.character + p.character - to.character,
            },
            p => Pos {
                line: p.line - to.line + end.line,
                character: p.character,
            },
        }
    }

    /// Move diagnostics with the edit and report line changes requiring message relayout.
    pub fn shift(&self, items: &mut [Diagnostic]) -> bool {
        let mut lines = false;
        for item in items {
            let start = self.moved(item.range.start);
            lines |= start.line != item.range.start.line;
            item.range.end = self.moved(item.range.end);
            item.range.start = start;
        }
        lines
    }

    /// Map unchanged-line messages to their new lines for [`render`] reuse.
    /// Exclude edited lines because their available width needs remeasurement.
    pub fn reline(&self, shown: Shown) -> Shown {
        let below = self.end.line as i32 - self.to.line as i32;
        shown
            .into_iter()
            .filter_map(|(line, up)| match line as u32 {
                l if l < self.from.line => Some((line, up)),
                l if l > self.to.line => Some((line + below, up)),
                _ => None,
            })
            .collect()
    }
}

/// Return all diagnostics covering `pos` for hover, including otherwise message-free hints.
pub fn at(items: &[Diagnostic], pos: Pos) -> Vec<&Diagnostic> {
    items
        .iter()
        .filter(|d| match d.range.start == d.range.end {
            // An empty range is what a server sends for "insert something here", and it covers
            // only the one position it sits on.
            true => pos == d.range.start,
            // Half-open, as everywhere: the character after the range is outside it.
            false => key(pos) >= key(d.range.start) && key(pos) < key(d.range.end),
        })
        .collect()
}

/// Format nonzero error/warning counts for the status bar, or `None` to hide an empty slot.
pub fn counts(items: &[Diagnostic]) -> Option<String> {
    let count = |want: Severity| items.iter().filter(|d| d.severity == want).count();
    let (errors, warnings) = (count(Severity::Error), count(Severity::Warning));
    let plural = |n: usize, one: &str| match n {
        1 => format!("1 {one}"),
        n => format!("{n} {one}s"),
    };
    match (errors, warnings) {
        (0, 0) => None,
        (e, 0) => Some(plural(e, "error")),
        (0, w) => Some(plural(w, "warning")),
        (e, w) => Some(format!("{}, {}", plural(e, "error"), plural(w, "warning"))),
    }
}

/// Every message on `line`, one per line: what the gutter icon says when the pointer rests on it.
pub fn messages_on(items: &[Diagnostic], line: u32) -> String {
    items
        .iter()
        .filter(|d| {
            matches!(d.severity, Severity::Error | Severity::Warning) && d.range.start.line == line
        })
        .map(|d| d.message.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use accent_api::Range;

    fn diag(severity: Severity, line: u32, from: u32, to: u32) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Pos {
                    line,
                    character: from,
                },
                end: Pos {
                    line,
                    character: to,
                },
            },
            severity,
            message: format!("{severity:?} at {line}:{from}"),
            source: None,
            code: None,
        }
    }

    #[test]
    fn counts_names_errors_and_warnings_and_nothing_else() {
        assert_eq!(counts(&[]), None);
        assert_eq!(counts(&[diag(Severity::Hint, 0, 0, 1)]), None);
        assert_eq!(
            counts(&[diag(Severity::Error, 0, 0, 1)]),
            Some("1 error".to_string())
        );
        assert_eq!(
            counts(&[
                diag(Severity::Error, 0, 0, 1),
                diag(Severity::Error, 1, 0, 1),
                diag(Severity::Warning, 2, 0, 1),
                diag(Severity::Info, 3, 0, 1),
            ]),
            Some("2 errors, 1 warning".to_string())
        );
    }

    #[test]
    fn at_takes_the_ranges_that_cover_the_position() {
        let items = [
            diag(Severity::Error, 1, 2, 6),
            diag(Severity::Hint, 1, 4, 4),
            diag(Severity::Warning, 2, 0, 3),
        ];
        let found = at(
            &items,
            Pos {
                line: 1,
                character: 4,
            },
        );
        assert_eq!(found.len(), 2, "the range and the empty one on the caret");
        // The end is exclusive, so the character after the range is outside it.
        assert!(
            at(
                &items,
                Pos {
                    line: 1,
                    character: 6
                }
            )
            .is_empty()
        );
    }

    #[test]
    fn a_gutter_icon_says_every_loud_message_on_its_line() {
        let items = [
            diag(Severity::Error, 3, 0, 1),
            diag(Severity::Warning, 3, 4, 5),
            diag(Severity::Hint, 3, 8, 9),
            diag(Severity::Error, 4, 0, 1),
        ];
        assert_eq!(
            messages_on(&items, 3),
            "Error at 3:0\nWarning at 3:4",
            "the hint is not on the icon, which is not drawn for it"
        );
    }

    #[test]
    fn an_edit_moves_what_follows_it_and_closes_up_what_it_removed() {
        let pos = |line, character| Pos { line, character };
        let ranges = |items: &[Diagnostic]| {
            let at = |p: Pos| (p.line, p.character);
            items
                .iter()
                .map(|d| (at(d.range.start), at(d.range.end)))
                .collect::<Vec<_>>()
        };
        let mut items = [
            diag(Severity::Error, 0, 0, 2),
            diag(Severity::Error, 2, 4, 6),
        ];
        let shift =
            |items: &mut [Diagnostic], from, to, text| Edit::new(from, to, text).shift(items);
        // A line typed in above the second: only it moves, by a line.
        assert!(shift(&mut items, pos(1, 0), pos(1, 0), "new\n"));
        assert_eq!(ranges(&items), [((0, 0), (0, 2)), ((3, 4), (3, 6))]);
        // Typed before it on its own line: along the line, no line moved.
        assert!(!shift(&mut items, pos(3, 1), pos(3, 1), "ab"));
        assert_eq!(ranges(&items), [((0, 0), (0, 2)), ((3, 6), (3, 8))]);
        // A deletion from inside the first to the second's line: the first closes up at the cut,
        // the second joins the first line.
        assert!(shift(&mut items, pos(0, 1), pos(3, 2), ""));
        assert_eq!(ranges(&items), [((0, 0), (0, 1)), ((0, 5), (0, 7))]);
    }

    #[test]
    fn a_message_too_long_for_its_room_ends_in_an_ellipsis() {
        let width = |s: &str| s.chars().count() as i32 * 10;
        let cut = |text: &str, room| ellipsize(text.to_string(), room, width);
        assert_eq!(cut("unbalanced group", 160), "unbalanced group");
        assert_eq!(cut("unbalanced group", 80), "unbalan…");
        // Cut after a word, the space goes rather than sitting before the ellipsis.
        assert_eq!(cut("unbalanced group", 120), "unbalanced…");
        // No room at all still says there was something.
        assert_eq!(cut("unbalanced group", 5), "…");
    }
}
