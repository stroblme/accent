//! Wrapped rows that carry on under their line's own indent rather than at the left margin, in
//! every text tab: behind a note's list or quote marker, and one indent level deeper than any other
//! indented line ([`typing::wrap_column`]).
//!
//! GtkTextView has no wrap indent of its own. What it has is a paragraph tag's `indent`, which
//! Pango reads as a hanging indent when it is negative: the first row stays where it was and every
//! wrapped row starts that many pixels right of it, the trick `highlight::hang` pulls heading
//! markers into the gutter with. A tag's indent is pixels and not a function of the line it lands
//! on, so there is one tag per column, `wrap1`..`wrap32`, each given its width in the view's font by
//! [`measure`]; a line that would hang deeper hangs at the last. A note's list and quote markers
//! hang at their own width rather than a column's, which in a proportional face is not their
//! character count in spaces: a tag per indent and marker, `wrap2:- `, made the first time a line
//! asks for it and measured with the marker laid out as the note draws it ([`marker_tag`]).
//!
//! A line's paragraph values are the ones on its first character, so that is the character
//! [`retag`] checks, and a tag goes on whole lines. [`follow`] keeps the tags in step with the text:
//! every line once, then only the lines each insertion or deletion touched, so a keystroke costs
//! the same in a long file as in a short one. A note's fenced block is code, and its lines wrap as
//! code does whatever they open with; [`refence`] follows the styling pass that finds the fences.

use crate::{highlight, typing};
use gtk::pango;
use gtk::prelude::*;
use sourceview5::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

/// The deepest column a wrap hangs at: a line seven levels into four-space code, or a paragraph
/// under three tabs in a note.
const COLUMNS: usize = 32;

/// How much of a line [`retag`] reads. Its column is capped at [`COLUMNS`], which this many
/// characters of indent always reach, with room for a marker behind a shallower one; a minified
/// file's one long line is not copied out on every keystroke.
const HEAD_CHARS: i32 = COLUMNS as i32 + 16;

fn name(column: usize) -> String {
    format!("wrap{column}")
}

/// A marker tag's name: `wrap2:- ` hangs a line behind a bullet two columns in.
fn marker_name(columns: usize, marker: &str) -> String {
    format!("wrap{columns}:{marker}")
}

/// The indent and marker a marker tag was made for, read back out of its name.
fn marker_of(name: &str) -> Option<(usize, &str)> {
    let (columns, marker) = name.strip_prefix("wrap")?.split_once(':')?;
    Some((columns.parse().ok()?, marker))
}

/// Whether `tag` is one of this module's, a column's or a marker's.
fn is_wrap(tag: &gtk::TextTag) -> bool {
    tag.name().is_some_and(|name| name.starts_with("wrap"))
}

thread_local! {
    /// Each marker's width in Pango units, by the font and the space's width it was laid out at:
    /// every restyle measures the tags again, and a font that has not changed is a lookup.
    static WIDTHS: RefCell<HashMap<(String, i32, String), i32>> = RefCell::new(HashMap::new());
}

/// Create the tags in `buffer`'s table (idempotent). Before anything a flavour installs: a note's
/// heading tags set an indent of their own, and a tag added later outranks an earlier one, which
/// is how an indented ATX heading keeps the hang that pulls its `#` markers out.
pub fn install(buffer: &sourceview5::Buffer) {
    let table = buffer.tag_table();
    for column in 1..=COLUMNS {
        if table.lookup(&name(column)).is_none() {
            table.add(&gtk::TextTag::new(Some(&name(column))));
        }
    }
}

/// Give every tag its width in `view`'s font, a space per column: the unit GtkSourceView sets its
/// tab stops in, so a tab-indented line lines up exactly, and in the monospaced face code always
/// has and a note has by default, every character's advance. A marker tag adds its marker as laid
/// out ([`hang`]). Called again on every font or zoom change. A tag whose width has not moved is
/// left alone, because setting one lays out again every line it is on.
pub fn measure(view: &sourceview5::View) {
    let space = space(view);
    let table = view.buffer().tag_table();
    for column in 1..=COLUMNS {
        let Some(tag) = table.lookup(&name(column)) else {
            continue;
        };
        let indent = -pango::units_to_double(space * column as i32).round() as i32;
        if tag.indent() != indent {
            tag.set_indent(indent);
        }
    }
    let mut marked = Vec::new();
    table.foreach(|tag| marked.push(tag.clone()));
    for tag in marked {
        let Some(name) = tag.name() else {
            continue;
        };
        if let Some((columns, marker)) = marker_of(&name) {
            let indent = hang(view, space, columns, marker);
            if tag.indent() != indent {
                tag.set_indent(indent);
            }
        }
    }
}

/// A space's width in `view`'s font, in Pango units.
fn space(view: &sourceview5::View) -> i32 {
    view.create_pango_layout(Some(" ")).size().0
}

/// The indent that hangs a line's wrapped rows behind `marker`, `columns` into the line: the
/// marker laid out in the view's font as the note draws it, its bullet or number bold (the
/// `listmarker` tag), a quote's `>` italic with the rest of the quote, the spaces after either
/// plain. In a monospaced face that is the marker's width in spaces, as a column tag's.
fn hang(view: &sourceview5::View, space: i32, columns: usize, marker: &str) -> i32 {
    let font = view
        .pango_context()
        .font_description()
        .map(|font| font.to_string())
        .unwrap_or_default();
    let key = (font, space, marker.to_string());
    let width = WIDTHS.with_borrow(|widths| widths.get(&key).copied());
    let width = width.unwrap_or_else(|| {
        let layout = view.create_pango_layout(Some(marker));
        let attrs = pango::AttrList::new();
        attrs.insert(match marker.starts_with('>') {
            true => pango::AttrInt::new_style(pango::Style::Italic).upcast(),
            false => {
                let mut bold = pango::AttrInt::new_weight(pango::Weight::Bold).upcast();
                bold.set_end_index(marker.trim_end_matches(' ').len() as u32);
                bold
            }
        });
        layout.set_attributes(Some(&attrs));
        let width = layout.size().0;
        WIDTHS.with_borrow_mut(|widths| widths.insert(key, width));
        width
    });
    -pango::units_to_double(space * columns as i32 + width).round() as i32
}

/// The tag a note's line hangs behind `marker` with, `columns` into it, made the first time a line
/// asks for it. Under every other tag, as the column tags are, so a heading's hang outranks it.
fn marker_tag(view: &sourceview5::View, columns: usize, marker: &str) -> gtk::TextTag {
    let table = view.buffer().tag_table();
    let name = marker_name(columns, marker);
    if let Some(tag) = table.lookup(&name) {
        return tag;
    }
    let tag = gtk::TextTag::new(Some(&name));
    table.add(&tag);
    tag.set_priority(0);
    tag.set_indent(hang(view, space(view), columns, marker));
    tag
}

/// Tag every line of `view` now, and from then on the lines each edit touches: an insertion's
/// lines, and the line a deletion joins. A new tab width or indent width moves every indented
/// line's column, so it retags them all. `markers` says the text is a note's, whose list and quote
/// markers set their own hang.
pub fn follow(view: &sourceview5::View, markers: bool) {
    let buffer = view.buffer();
    let tags = tags(&buffer);
    retag(view, &tags, markers, 0, buffer.line_count() - 1);
    // After the default handlers, which leave the insertion's iter behind the inserted text and a
    // deletion's two on the join. Weak: the view holds the buffer, which holds these closures.
    let (weak, all) = (view.downgrade(), tags.clone());
    buffer.connect_local("insert-text", true, move |args| {
        let view = weak.upgrade()?;
        let end = args[1].get::<gtk::TextIter>().ok()?;
        let chars = args[2].get::<&str>().ok()?.chars().count() as i32;
        let first = view.buffer().iter_at_offset(end.offset() - chars).line();
        retag(&view, &all, markers, first, end.line());
        None
    });
    let (weak, all) = (view.downgrade(), tags.clone());
    buffer.connect_local("delete-range", true, move |args| {
        let view = weak.upgrade()?;
        let line = args[1].get::<gtk::TextIter>().ok()?.line();
        retag(&view, &all, markers, line, line);
        None
    });
    for property in ["tab-width", "indent-width"] {
        let all = tags.clone();
        view.connect_notify_local(Some(property), move |view, _| {
            retag(view, &all, markers, 0, view.buffer().line_count() - 1);
        });
    }
}

/// The wrap tags of `buffer`'s table, `wrap1` first.
fn tags(buffer: &gtk::TextBuffer) -> Rc<[gtk::TextTag]> {
    let table = buffer.tag_table();
    (1..=COLUMNS)
        .filter_map(|column| table.lookup(&name(column)))
        .collect()
}

/// Run `restyle`, a note's styling pass, then retag the lines it put into a fence or took out of
/// one. The pass moves the `codeblock` tag a fence's lines are known by without an edit on them —
/// typing a fence open takes in every line below it — so no insertion or deletion retags them.
/// Only the runs of fenced lines that differ before and after are retagged: a pass that opens or
/// closes nothing costs two walks over the tag's toggles.
pub fn refence<T>(view: &sourceview5::View, restyle: impl FnOnce() -> T) -> T {
    let before = fences(view);
    let styled = restyle();
    let after = fences(view);
    let tags = tags(&view.buffer());
    for lines in moved(&before, &after) {
        retag(view, &tags, true, lines.start, lines.end - 1);
    }
    styled
}

/// The runs of lines in a fence, as the `codeblock` tag lies now: a line is in one when its first
/// character is, the character its wrap is read from.
fn fences(view: &sourceview5::View) -> Vec<Range<i32>> {
    let buffer = view.buffer();
    let Some(tag) = buffer.tag_table().lookup(highlight::CODEBLOCK) else {
        return Vec::new();
    };
    // A line counts from its own start, so a run that starts or ends inside a line starts at the
    // next and ends at that one.
    let line = |at: &gtk::TextIter| at.line() + i32::from(!at.starts_line());
    let (mut runs, mut at) = (Vec::new(), buffer.start_iter());
    while at.has_tag(&tag) || at.forward_to_tag_toggle(Some(&tag)) {
        let first = line(&at);
        at.forward_to_tag_toggle(Some(&tag));
        let end = line(&at);
        if first < end {
            runs.push(first..end);
        }
    }
    runs
}

/// The runs that differ between two lists of them: every run of either past what the two have
/// in common at the front and at the back. A line in none of them is in a fence in both or in
/// neither.
fn moved<'a>(
    before: &'a [Range<i32>],
    after: &'a [Range<i32>],
) -> impl Iterator<Item = &'a Range<i32>> {
    let head = before.iter().zip(after).take_while(|(b, a)| b == a).count();
    let (before, after) = (&before[head..], &after[head..]);
    let tail = before
        .iter()
        .rev()
        .zip(after.iter().rev())
        .take_while(|(b, a)| b == a)
        .count();
    before[..before.len() - tail]
        .iter()
        .chain(&after[..after.len() - tail])
}

/// Give each of lines `first..=last` the tag its column wants.
///
/// An edit's few lines are checked one by one, and only a line whose first character carries the
/// wrong tag is touched. A longer range, a whole file's or a paste's, loses the tags it has in one
/// removal each, is read as one string and gets its tags back a run of equally deep lines at a
/// time: past as many lines as there are tags, that is the cheaper of the two.
fn retag(view: &sourceview5::View, tags: &[gtk::TextTag], markers: bool, first: i32, last: i32) {
    let buffer = view.buffer();
    let tab_width = view.tab_width() as usize;
    // GtkSourceView's own "one indent": the indent width, or the tab width while that is unset.
    let level = match view.indent_width() {
        width if width > 0 => width as usize,
        _ => tab_width,
    };
    // A fence's lines are code, so what they open with is no marker ([`refence`]).
    let fence = markers
        .then(|| buffer.tag_table().lookup(highlight::CODEBLOCK))
        .flatten();
    let want = |text: &str, line: i32| {
        let fenced = fence.as_ref().is_some_and(|tag| {
            buffer
                .iter_at_line(line)
                .is_some_and(|start| start.has_tag(tag))
        });
        let markers = markers && !fenced;
        let column = typing::wrap_column(text, tab_width, level, markers);
        match typing::wrap_head(text, tab_width, markers) {
            (columns, marker) if !marker.is_empty() && column <= COLUMNS => {
                Some(marker_tag(view, columns, marker))
            }
            _ => column
                .min(COLUMNS)
                .checked_sub(1)
                .and_then(|index| tags.get(index))
                .cloned(),
        }
    };
    let line_end = |line| crate::editor::line_end(&buffer, line);
    if last - first < COLUMNS as i32 {
        for line in first..=last {
            let Some(start) = buffer.iter_at_line(line) else {
                break;
            };
            let end = line_end(line);
            let mut head = start;
            head.forward_chars(HEAD_CHARS);
            let want = want(&buffer.text(&start, &head.min(end), true), line);
            let have: Vec<gtk::TextTag> = start.tags().into_iter().filter(is_wrap).collect();
            if have.len() == usize::from(want.is_some())
                && want.as_ref().is_none_or(|tag| have.contains(tag))
            {
                continue;
            }
            for tag in &have {
                buffer.remove_tag(tag, &start, &end);
            }
            if let Some(tag) = want {
                buffer.apply_tag(&tag, &start, &end);
            }
        }
        return;
    }
    let Some(start) = buffer.iter_at_line(first) else {
        return;
    };
    let end = line_end(last);
    // Only the tags that are there: a removal lays out every line of the range again, whether it
    // took anything off or not, and freshly inserted text usually has none.
    let mut all = Vec::new();
    buffer.tag_table().foreach(|tag| {
        if is_wrap(tag) {
            all.push(tag.clone());
        }
    });
    for tag in &all {
        let mut toggle = start;
        if start.has_tag(tag) || (toggle.forward_to_tag_toggle(Some(tag)) && toggle < end) {
            buffer.remove_tag(tag, &start, &end);
        }
    }
    // `lines` ends a line where the buffer does, the file's CRLF being `\n` by the time it is here.
    let wants: Vec<_> = buffer
        .text(&start, &end, true)
        .lines()
        .zip(first..)
        .map(|(text, line)| want(text, line))
        .collect();
    let mut from = 0;
    for to in 1..=wants.len() {
        if to < wants.len() && wants[to] == wants[from] {
            continue;
        }
        if let (Some(tag), Some(start)) = (&wants[from], buffer.iter_at_line(first + from as i32)) {
            buffer.apply_tag(tag, &start, &line_end(first + to as i32 - 1));
        }
        from = to;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only the fences a pass moved are retagged: what the two lists share at either end is left
    /// alone, and everything between, from both, is not.
    #[test]
    // A run of lines is a range, and a list of one of them is what this is about.
    #[allow(clippy::single_range_in_vec_init)]
    fn only_the_fences_that_moved_are_retagged() {
        let moved = |before: &[Range<i32>], after: &[Range<i32>]| {
            moved(before, after).cloned().collect::<Vec<_>>()
        };
        assert!(moved(&[2..5, 9..12], &[2..5, 9..12]).is_empty());
        // Opened on a note's first run: that fence and the rest of the file.
        assert_eq!(moved(&[], &[3..40]), [3..40]);
        // Closed again, which gives the lines below back.
        assert_eq!(moved(&[3..40], &[3..8]), [3..40, 3..8]);
        // A fence opened between two others: the ones either side stay put.
        assert_eq!(moved(&[0..2, 20..24], &[0..2, 10..14, 20..24]), [10..14]);
    }
}
