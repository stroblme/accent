//! Indent wrapped rows behind list or quote markers, or one level beyond a line's indent
//! ([`typing::wrap_column`]).
//!
//! GtkTextView has no wrap-indent API; negative paragraph indents provide hanging indents.
//! Column tags (`wrap1`..`wrap32`) use widths measured in the view's font. Marker tags measure
//! the styled marker itself, so proportional fonts align correctly; deeper columns are capped.
//! A line indented with a tab takes a marker tag, with or without a marker: GtkSourceView puts its
//! tab stops a whole number of pixels apart, so a tab's columns are not as wide as as many spaces.
//!
//! Paragraph properties come from the first character, so tags cover whole lines and
//! [`retag`] checks that character. [`follow`] retags only edited lines, keeping keystroke
//! work independent of file length. [`refence`] follows styling changes so fenced lines
//! wrap as code even when their text was not edited.

use crate::{highlight, typing};
use gtk::pango;
use gtk::prelude::*;
use sourceview5::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

/// Deepest supported wrap column.
const COLUMNS: usize = 32;

/// Maximum prefix read by [`retag`], enough to reach [`COLUMNS`] plus a marker.
/// Bounding it avoids copying minified lines on each keystroke.
const HEAD_CHARS: i32 = COLUMNS as i32 + 16;

fn name(column: usize) -> String {
    format!("wrap{column}")
}

/// A marker tag's name: `wrap1t2:- ` hangs a line behind a bullet two columns past the first tab
/// stop, `wrap1t4:` four columns past it behind nothing.
fn marker_name(stops: usize, columns: usize, marker: &str) -> String {
    format!("wrap{stops}t{columns}:{marker}")
}

/// The tab stops, columns and marker a marker tag was made for, read back out of its name.
fn marker_of(name: &str) -> Option<(usize, usize, &str)> {
    let (indent, marker) = name.strip_prefix("wrap")?.split_once(':')?;
    let (stops, columns) = indent.split_once('t')?;
    Some((stops.parse().ok()?, columns.parse().ok()?, marker))
}

/// Whether `tag` is one of this module's, a column's or a marker's.
fn is_wrap(tag: &gtk::TextTag) -> bool {
    tag.name().is_some_and(|name| name.starts_with("wrap"))
}

thread_local! {
    /// Cached marker widths in Pango units, keyed by font and space width to reuse unchanged restyles.
    static WIDTHS: RefCell<HashMap<(String, i32, String), i32>> = RefCell::new(HashMap::new());
}

/// Create wrap tags before flavour tags so heading indentation outranks them. Idempotent.
pub fn install(buffer: &sourceview5::Buffer) {
    let table = buffer.tag_table();
    for column in 1..=COLUMNS {
        if table.lookup(&name(column)).is_none() {
            table.add(&gtk::TextTag::new(Some(&name(column))));
        }
    }
}

/// Measure tags in `view`'s font, using spaces for columns and [`hang`] for marker widths.
/// Remeasure after font, zoom or tab width changes, updating only changed widths to avoid line
/// relayout.
pub fn measure(view: &sourceview5::View) {
    let (space, tab) = (space(view), tab(view));
    let table = view.buffer().tag_table();
    for column in 1..=COLUMNS {
        let Some(tag) = table.lookup(&name(column)) else {
            continue;
        };
        let indent = indent(0, 0, column, space, 0);
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
        if let Some((stops, columns, marker)) = marker_of(&name) {
            let indent = hang(view, (space, tab), stops, columns, marker);
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

/// GtkSourceView's distance between tab stops, in whole pixels: `tab-width` spaces in `view`'s
/// font, rounded up as `pango_layout_get_pixel_size` rounds (`set_tab_stops_internal`).
fn tab(view: &sourceview5::View) -> i32 {
    let spaces = " ".repeat(view.tab_width() as usize);
    view.create_pango_layout(Some(&spaces)).pixel_size().0
}

/// The tab stops a line's indent reaches by its last tab, as columns count them.
fn tab_stops(line: &str, tab_width: usize) -> usize {
    let indent = &line[..line.len() - line.trim_start_matches([' ', '\t']).len()];
    indent.rfind('\t').map_or(0, |last| {
        typing::wrap_head(&indent[..=last], tab_width, false).0 / tab_width
    })
}

/// A paragraph indent hanging rows behind `stops` tab stops `tab` pixels apart, then `columns`
/// spaces `space` wide and a marker `width` wide, both in Pango units: negative, in whole pixels.
fn indent(stops: usize, tab: i32, columns: usize, space: i32, width: i32) -> i32 {
    let units = stops as i32 * tab * pango::SCALE + columns as i32 * space + width;
    -pango::units_to_double(units).round() as i32
}

/// Hanging indent behind `marker` at `stops` tab stops and `columns` past them, measured with the
/// note's styled marker. Bullets/numbers are bold, quote markers italic and following spaces plain.
fn hang(
    view: &sourceview5::View,
    (space, tab): (i32, i32),
    stops: usize,
    columns: usize,
    marker: &str,
) -> i32 {
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
    indent(stops, tab, columns, space, width)
}

/// Create a marker wrap tag lazily, below other tags so heading indentation wins.
fn marker_tag(
    view: &sourceview5::View,
    stops: usize,
    columns: usize,
    marker: &str,
) -> gtk::TextTag {
    let table = view.buffer().tag_table();
    let name = marker_name(stops, columns, marker);
    if let Some(tag) = table.lookup(&name) {
        return tag;
    }
    let tag = gtk::TextTag::new(Some(&name));
    table.add(&tag);
    tag.set_priority(0);
    tag.set_indent(hang(view, (space(view), tab(view)), stops, columns, marker));
    tag
}

/// Tag current lines and retag insertion/deletion lines. Retag all after tab or indent width changes.
/// `markers` enables note-specific list and quote indentation.
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
            // The tags behind a tab hang by its stops, which are as wide as the tab width's spaces.
            measure(view);
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

/// Run `restyle` and retag only changed fenced-line runs.
/// Styling can change fence membership without editing those lines, so normal edit hooks miss it.
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

/// Return runs differing after trimming shared prefixes and suffixes.
/// Other lines are fenced in both lists or neither.
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

/// Retag lines `first..=last` for their wrap columns.
///
/// Small edits update only incorrect tags. Large ranges remove existing tags once and apply
/// runs of equal indentation, which is cheaper beyond the number of tags.
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
        let (_, marker) = typing::wrap_head(text, tab_width, markers);
        let stops = tab_stops(text, tab_width);
        match (stops > 0 || !marker.is_empty()) && column <= COLUMNS {
            // What the row hangs behind past the stops: the indent's own columns, then the marker
            // or one level.
            true => {
                let columns = column - stops * tab_width - marker.len();
                Some(marker_tag(view, stops, columns, marker))
            }
            false => column
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

    /// A tab hangs a row from GtkSourceView's stop, whole pixels after the last, not from as many
    /// spaces: under Adwaita Sans 11 a space is 4.125 px, four of them 16.5 and the stop 17, so
    /// `\t- `'s text, behind a 10.75 px marker, starts at 27.75 and its rows at 28, where four
    /// spaces would hang them at 27.
    #[test]
    fn a_tab_hangs_from_its_stop() {
        let (space, tab, marker) = (4224, 17, 11008);
        assert_eq!(indent(1, tab, 0, space, marker), -28);
        assert_eq!(indent(0, tab, 4, space, marker), -27);
        assert_eq!(indent(2, tab, 4, space, 0), -51, "two stops, then a level");
        assert_eq!(tab_stops("\t- item", 4), 1);
        assert_eq!(tab_stops("\t\t  - item", 4), 2, "spaces after are columns");
        assert_eq!(tab_stops("  \t- item", 4), 1, "after spaces too");
        assert_eq!(tab_stops("    - item", 4), 0);
        assert_eq!(tab_stops("text\tafter", 4), 0, "only the indent counts");
    }

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
