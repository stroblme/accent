//! Shared caret, line-end and line-prefix queries.
//!
//! Generic buffers support both `sourceview5::Buffer` and widget-provided `GtkTextBuffer`s.

use gtk::glib;
use gtk::prelude::*;

/// The primary caret.
pub(crate) fn caret(buffer: &impl IsA<gtk::TextBuffer>) -> gtk::TextIter {
    let buffer = buffer.as_ref();
    buffer.iter_at_mark(&buffer.get_insert())
}

/// Return the clamped line end before its newline. Avoid `forward_to_line_end`
/// on empty lines, where it would advance to the next line.
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

/// Text from the line start to `iter`, including indent and list marker.
pub(crate) fn line_prefix(
    buffer: &impl IsA<gtk::TextBuffer>,
    iter: &gtk::TextIter,
) -> glib::GString {
    let mut start = *iter;
    start.set_line_offset(0);
    buffer.as_ref().text(&start, iter, true)
}
