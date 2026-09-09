//! The three questions every part of the editor asks a buffer: where the caret is, where a line
//! ends, and what the caret has in front of it on its own line.
//!
//! Generic over the buffer type, because half the callers hold a `sourceview5::Buffer` and the
//! other half the plain `GtkTextBuffer` a widget hands them.

use gtk::glib;
use gtk::prelude::*;

/// The primary caret.
pub(crate) fn caret(buffer: &impl IsA<gtk::TextBuffer>) -> gtk::TextIter {
    let buffer = buffer.as_ref();
    buffer.iter_at_mark(&buffer.get_insert())
}

/// The end of `line`, before its newline, clamped to what the buffer has.
///
/// `forward_to_line_end` runs on to the next line from an empty one, so a line that is already at
/// its end is left alone.
pub(crate) fn line_end(buffer: &impl IsA<gtk::TextBuffer>, line: i32) -> gtk::TextIter {
    let buffer = buffer.as_ref();
    let line = line.clamp(0, (buffer.line_count() - 1).max(0));
    let mut at = buffer
        .iter_at_line(line)
        .unwrap_or_else(|| buffer.end_iter());
    if !at.ends_line() {
        at.forward_to_line_end();
    }
    at
}

/// The text on `iter`'s line up to `iter`: the indent, the list marker and whatever has been
/// typed after them.
pub(crate) fn line_prefix(
    buffer: &impl IsA<gtk::TextBuffer>,
    iter: &gtk::TextIter,
) -> glib::GString {
    let mut start = *iter;
    start.set_line_offset(0);
    buffer.as_ref().text(&start, iter, true)
}
