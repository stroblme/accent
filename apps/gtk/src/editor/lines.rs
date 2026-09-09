//! What a tab does to whole lines: the clipboard's line cut and copy, Insert Line Below,
//! duplicate and delete, the comment toggle, wrapping, and the template snippets whose Tab stops
//! are walked through the text they inserted.

use super::{Tab, caret, line_end};
use crate::comment;
use gtk::prelude::*;
use sourceview5::prelude::*;

/// The spaces and tabs a line opens with, which is what a line inserted below it copies.
fn leading_indent(line: &str) -> &str {
    let end = line
        .find(|c: char| c != ' ' && c != '\t')
        .unwrap_or(line.len());
    &line[..end]
}

/// The text a duplicated line is inserted as. A line that already ends in a newline can be
/// repeated as it stands; the last line of a file has none, so the copy brings its own.
fn duplicated(line: &str) -> String {
    match line.ends_with('\n') {
        true => line.to_string(),
        false => format!("\n{line}"),
    }
}

/// A line as the clipboard should carry it: with the newline back that a last line does not have
/// of its own, so pasting it opens a line rather than splicing into the one under the caret.
fn paste_ready(line: &str) -> String {
    match line.ends_with('\n') {
        true => line.to_string(),
        false => format!("{line}\n"),
    }
}

/// The caret's line, from its start to the start of the next one, so the trailing newline is part
/// of it except on a last line that has none.
pub(super) fn line_bounds(buffer: &gtk::TextBuffer) -> (gtk::TextIter, gtk::TextIter) {
    let mut start = caret(buffer);
    start.set_line_offset(0);
    let mut end = start;
    // On the last line this lands on the end of the buffer and reports failure, which is
    // exactly where the line ends, so the answer is the same either way.
    end.forward_line();
    (start, end)
}

/// VS Code's whole-line cut and copy: with nothing selected, `Ctrl+X` and `Ctrl+C` take the
/// caret's whole line, its newline with it, so a later paste puts a line back instead of a
/// fragment.
///
/// No key handling, and no accelerator either — DESIGN.md's never-bind list keeps `Ctrl+X`/`C`
/// for the widget. Both chords emit these two signals, and `gtk_text_buffer_cut_clipboard` and
/// `..._copy_clipboard` do nothing at all without a selection, so each handler runs before an
/// inherited one that is then a no-op and does the work itself. Selecting the line and letting
/// the default handler have it instead would work for the cut and leave the copy selected.
///
/// After a cut the caret is where the deletion left it, at the start of the following line;
/// VS Code lands on the same line but keeps the column.
pub(super) fn line_clipboard(view: &sourceview5::View) {
    view.connect_copy_clipboard(|view| {
        let buffer = view.buffer();
        if buffer.has_selection() {
            return;
        }
        let (start, end) = line_bounds(&buffer);
        view.clipboard()
            .set_text(&paste_ready(&buffer.text(&start, &end, true)));
    });
    view.connect_cut_clipboard(|view| {
        let buffer = view.buffer();
        if buffer.has_selection() || !view.is_editable() {
            return;
        }
        let (mut start, mut end) = line_bounds(&buffer);
        let line = buffer.text(&start, &end, true);
        view.clipboard().set_text(&paste_ready(&line));
        // A last line with no newline of its own takes the one above it, or the cut leaves the
        // blank line it used to sit on. `delete_line` does the same.
        if !line.ends_with('\n') {
            start.backward_char();
        }
        buffer.begin_user_action();
        buffer.delete(&mut start, &mut end);
        buffer.end_user_action();
    });
}

/// `text` as a snippet whose stops are the byte offsets `stops`, in Tab order.
fn snippet(text: &str, stops: &[usize]) -> sourceview5::Snippet {
    let snippet = sourceview5::Snippet::new(None, None);
    for (piece, focus) in chunks(text, stops) {
        let chunk = sourceview5::SnippetChunk::new();
        chunk.set_text(piece);
        chunk.set_text_set(true);
        chunk.set_focus_position(focus);
        snippet.add_chunk(&chunk);
    }
    snippet
}

/// The text between the stops, then each stop as an empty chunk numbered from one; plain text
/// carries -1, which is what GtkSourceView reads as "not a stop". A stop off a character
/// boundary, which the renderer never produces, is skipped rather than trusted.
fn chunks<'a>(text: &'a str, stops: &[usize]) -> Vec<(&'a str, i32)> {
    let mut out = Vec::new();
    let mut byte = 0;
    let mut focus = 0;
    for &stop in stops {
        let Some(piece) = text.get(byte..stop) else {
            continue;
        };
        if !piece.is_empty() {
            out.push((piece, -1));
        }
        focus += 1;
        out.push(("", focus));
        byte = stop;
    }
    if byte < text.len() {
        out.push((&text[byte..], -1));
    }
    out
}

impl Tab {
    /// Comment or uncomment the selected lines with the language's own markers.
    ///
    /// GtkSourceView carries the markers in the language's metadata but does no toggling of its
    /// own, so the text goes out to [`crate::comment`] and comes back as one replacement, inside
    /// a single user action so one Ctrl+Z undoes the whole thing.
    pub fn toggle_comment(&self) {
        let Some(language) = self.buffer.language() else {
            return;
        };
        let had_selection = self.buffer.has_selection();
        let (mut start, mut end) = match self.buffer.selection_bounds() {
            Some(bounds) => bounds,
            None => {
                let at = caret(&self.buffer);
                (at, at)
            }
        };
        // Whole lines: a marker goes in front of a line, never in front of a word.
        start.set_line_offset(0);
        end = line_end(&self.buffer, end.line());
        let text = self.buffer.text(&start, &end, true);
        let toggled = match language.metadata("line-comment-start") {
            Some(marker) => comment::toggle_lines(&text, &marker),
            None => {
                let (Some(open), Some(close)) = (
                    language.metadata("block-comment-start"),
                    language.metadata("block-comment-end"),
                ) else {
                    return;
                };
                comment::toggle_block(&text, &open, &close)
            }
        };
        let anchor = start.offset();
        self.buffer.begin_user_action();
        self.buffer.delete(&mut start, &mut end);
        self.buffer.insert(&mut start, &toggled);
        self.buffer.end_user_action();
        if had_selection {
            self.buffer
                .select_range(&self.buffer.iter_at_offset(anchor), &start);
        }
    }

    /// Wrap long lines, or stop. Every tab starts wrapped and can be told otherwise for as long
    /// as it is open, which is the escape hatch for a file whose columns are the point.
    pub fn toggle_wrap(&self) {
        self.view.set_wrap_mode(match self.view.wrap_mode() {
            gtk::WrapMode::None => gtk::WrapMode::WordChar,
            _ => gtk::WrapMode::None,
        });
    }

    // --- line operations -----------------------------------------------------------------

    /// The caret's whole line; see [`line_bounds`], which the clipboard handlers share.
    fn line_bounds(&self) -> (gtk::TextIter, gtk::TextIter) {
        line_bounds(self.buffer.upcast_ref())
    }

    /// VS Code's Insert Line Below: open a line under the caret's and put the caret on it, at the
    /// same indent, so a list item or an indented block carries on where it was. That is the idiom
    /// `typing.rs` already uses on Return; continuing the marker itself is Return's job, not this
    /// one's, because this is also how a line is opened *out* of a list.
    pub fn newline_below(&self) {
        let (start, end) = self.line_bounds();
        let line = self.buffer.text(&start, &end, true);
        let indent = leading_indent(&line).to_string();
        // Insert before the line's own newline, or at the end of the buffer on a last line that
        // has none. One user action, so one Ctrl+Z takes the whole line back.
        let mut at = end;
        if line.ends_with('\n') {
            at.backward_char();
        }
        self.buffer.begin_user_action();
        self.buffer.insert(&mut at, &format!("\n{indent}"));
        self.buffer.end_user_action();
        self.buffer.place_cursor(&at);
        self.view.scroll_mark_onscreen(&self.buffer.get_insert());
    }

    pub fn duplicate_line(&self) {
        let (start, mut end) = self.line_bounds();
        let line = self.buffer.text(&start, &end, true);
        self.buffer.begin_user_action();
        self.buffer.insert(&mut end, &duplicated(&line));
        self.buffer.end_user_action();
    }

    pub fn delete_line(&self) {
        let (mut start, mut end) = self.line_bounds();
        // A last line with no newline of its own takes the one separating it from the line
        // above, or deleting it would leave the blank line it used to sit on.
        if !self.buffer.text(&start, &end, true).ends_with('\n') {
            start.backward_char();
        }
        self.buffer.begin_user_action();
        self.buffer.delete(&mut start, &mut end);
        self.buffer.end_user_action();
    }

    /// Put the caret on a template's first `{{cursor}}` and make the rest Tab stops.
    ///
    /// GtkSourceView can only walk stops through text a snippet inserted, so with more than one
    /// the note's text is put back through a snippet: the same bytes, not undoable and not dirty,
    /// because nothing about the file changed. `byte` offsets, as the template renderer counts.
    pub fn place_stops(&self, stops: &[usize]) {
        let text = self.text();
        match stops {
            [] => {}
            [only] => {
                let at = text.get(..*only).map_or(0, |s| s.chars().count());
                self.goto_range(at..at);
            }
            _ => {
                let (mut start, mut end) = self.buffer.bounds();
                self.loading.set(true);
                self.buffer.begin_irreversible_action();
                self.buffer.delete(&mut start, &mut end);
                self.push_snippet(&snippet(&text, stops), &mut self.buffer.start_iter());
                self.buffer.end_irreversible_action();
                self.loading.set(false);
                self.buffer.set_modified(false);
                self.analyse();
            }
        }
    }

    /// A template's text at the caret, with its `{{cursor}}` stops for Tab to walk.
    pub fn insert_stops(&self, text: &str, stops: &[usize]) {
        let mut at = caret(&self.buffer);
        match stops.is_empty() {
            true => self.buffer.insert(&mut at, text),
            false => self.push_snippet(&snippet(text, stops), &mut at),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_split_the_text_at_its_stops_in_order() {
        assert_eq!(
            chunks("ab: \ncd: \n", &[4, 9]),
            [("ab: ", -1), ("", 1), ("\ncd: ", -1), ("", 2), ("\n", -1)]
        );
        assert_eq!(chunks("x", &[0]), [("", 1), ("x", -1)]);
        assert_eq!(chunks("x", &[1]), [("x", -1), ("", 1)]);
        assert_eq!(chunks("ab", &[0, 0]), [("", 1), ("", 2), ("ab", -1)]);
    }

    #[test]
    fn leading_indent_is_the_spaces_and_tabs_a_line_opens_with() {
        assert_eq!(leading_indent(""), "");
        assert_eq!(leading_indent("  - a"), "  ");
        assert_eq!(leading_indent("\tx"), "\t");
        assert_eq!(leading_indent("no indent\n"), "");
        assert_eq!(
            leading_indent("   \n"),
            "   ",
            "a blank line still has its indent"
        );
    }

    /// What a whole-line cut or copy puts on the clipboard: a line, newline included, so the
    /// paste that follows it opens a line of its own.
    #[test]
    fn a_copied_line_carries_its_newline() {
        assert_eq!(paste_ready("- item\n"), "- item\n");
        assert_eq!(
            paste_ready("last line"),
            "last line\n",
            "a last line has none"
        );
        assert_eq!(paste_ready("\n"), "\n", "an empty line is still a line");
    }

    #[test]
    fn a_duplicated_line_brings_its_own_newline_only_when_it_has_none() {
        assert_eq!(duplicated("note\n"), "note\n");
        assert_eq!(duplicated("last line"), "\nlast line");
        assert_eq!(duplicated(""), "\n");
    }
}
