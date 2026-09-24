//! Wrapped rows that carry on under their line's own indent rather than at the left margin, in
//! every text tab: behind a note's list or quote marker, and one indent level deeper than any other
//! indented line ([`typing::wrap_column`]).
//!
//! GtkTextView has no wrap indent of its own. What it has is a paragraph tag's `indent`, which
//! Pango reads as a hanging indent when it is negative: the first row stays where it was and every
//! wrapped row starts that many pixels right of it, the trick `highlight::hang` pulls heading
//! markers into the gutter with. A tag's indent is pixels and not a function of the line it lands
//! on, so there is one tag per column, `wrap1`..`wrap32`, each given its width in the view's font by
//! [`measure`]; a line that would hang deeper hangs at the last.
//!
//! A line's paragraph values are the ones on its first character, so that is the character
//! [`retag`] checks, and a tag goes on whole lines. [`follow`] keeps the tags in step with the text:
//! every line once, then only the lines each insertion or deletion touched, so a keystroke costs
//! the same in a long file as in a short one.

use crate::typing;
use gtk::pango;
use gtk::prelude::*;
use sourceview5::prelude::*;
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
/// has and a note has by default, every character's advance. Called again on every font or zoom
/// change. A tag whose width has not moved is left alone, because setting one lays out again every
/// line it is on.
pub fn measure(view: &sourceview5::View) {
    let space = view.create_pango_layout(Some(" ")).size().0;
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
}

/// Tag every line of `view` now, and from then on the lines each edit touches: an insertion's
/// lines, and the line a deletion joins. A new tab width or indent width moves every indented
/// line's column, so it retags them all. `markers` says the text is a note's, whose list and quote
/// markers set their own hang.
pub fn follow(view: &sourceview5::View, markers: bool) {
    let buffer = view.buffer();
    let table = buffer.tag_table();
    let tags: Rc<[gtk::TextTag]> = (1..=COLUMNS)
        .filter_map(|column| table.lookup(&name(column)))
        .collect();
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
    let want = |line: &str| {
        let column = typing::wrap_column(line, tab_width, level, markers).min(COLUMNS);
        column.checked_sub(1).and_then(|index| tags.get(index))
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
            let want = want(&buffer.text(&start, &head.min(end), true));
            let have: Vec<gtk::TextTag> = start
                .tags()
                .into_iter()
                .filter(|tag| tags.contains(tag))
                .collect();
            if have.len() == usize::from(want.is_some())
                && want.is_none_or(|tag| have.contains(tag))
            {
                continue;
            }
            for tag in &have {
                buffer.remove_tag(tag, &start, &end);
            }
            if let Some(tag) = want {
                buffer.apply_tag(tag, &start, &end);
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
    for tag in tags {
        let mut toggle = start;
        if start.has_tag(tag) || (toggle.forward_to_tag_toggle(Some(tag)) && toggle < end) {
            buffer.remove_tag(tag, &start, &end);
        }
    }
    // `lines` ends a line where the buffer does, the file's CRLF being `\n` by the time it is here.
    let wants: Vec<_> = buffer.text(&start, &end, true).lines().map(want).collect();
    let mut from = 0;
    for to in 1..=wants.len() {
        if to < wants.len() && wants[to] == wants[from] {
            continue;
        }
        if let (Some(tag), Some(start)) = (wants[from], buffer.iter_at_line(first + from as i32)) {
            buffer.apply_tag(tag, &start, &line_end(first + to as i32 - 1));
        }
        from = to;
    }
}
