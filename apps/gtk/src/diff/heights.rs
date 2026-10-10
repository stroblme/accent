//! What a column knows of its lines' natural heights from one relayout to the next, so a pass
//! measures only the lines something has changed since: see `Columns::relayout`.
//!
//! A line's height changes with its text, with a tag that sizes text applied to it or taken off
//! it, with such a tag's own properties, and with whatever lays out every line: the width, the
//! margins, the tab width, the line spacing, the font. The first three are heard as they happen
//! ([`listen`]), the last is compared on every pass ([`Known::ready`]). The padding the comparison
//! lays itself and the runs it hides are not changes of a line's own height.

use adw::prelude::*;
use gtk::glib;
use sourceview5::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::TAG_GAP;
use super::pad::is_pad;

/// A line's natural height as a column knows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Height {
    /// Never measured, or its text has changed since.
    Unknown,
    /// A figure GTK has still to confirm: one measured in a layout of our own, or one from before
    /// something GTK has to lay the line out again for — a tag that sizes text, a new layout, its
    /// padding, the line shown again.
    Pending(i32),
    /// GTK's own figure, nothing having changed since.
    Laid(i32),
}

impl Height {
    /// What `measure` answered: an estimate stays pending.
    pub(super) fn of(px: i32, estimate: bool) -> Self {
        match estimate {
            true => Height::Pending(px),
            false => Height::Laid(px),
        }
    }

    pub(super) fn pending(self) -> Self {
        match self {
            Height::Laid(px) => Height::Pending(px),
            other => other,
        }
    }
}

/// `removed` lines from `line` on replaced by `added` unknown ones: what an edit does to the
/// lines it touches. The rest keep their figures.
pub(super) fn splice(lines: &mut Vec<Height>, line: usize, removed: usize, added: usize) {
    let end = (line + removed).min(lines.len());
    lines.splice(
        line.min(end)..end,
        std::iter::repeat_n(Height::Unknown, added),
    );
}

/// What lays out every line of a view: a change of any of it rewraps or resizes them all.
#[derive(Clone, PartialEq, Debug)]
struct Layout {
    /// The text's width, the left and right margins, the indent, and the spacing above, below
    /// and inside a wrapped line.
    sizes: [i32; 7],
    wrap: gtk::WrapMode,
    tab: u32,
    font: Option<String>,
}

impl Layout {
    fn of(view: &sourceview5::View) -> Self {
        Layout {
            sizes: [
                view.visible_rect().width(),
                view.left_margin(),
                view.right_margin(),
                view.indent(),
                view.pixels_above_lines(),
                view.pixels_below_lines(),
                view.pixels_inside_wrap(),
            ],
            wrap: view.wrap_mode(),
            tab: view.tab_width(),
            font: (view.pango_context().font_description()).map(|font| font.to_string()),
        }
    }
}

/// One column's heights, by line of its buffer. A cell each, because the handlers [`listen`]
/// connects run while a pass holds the lines: a tag the pass lays is looked up in `sizes` and,
/// being the comparison's own, touches nothing else.
#[derive(Default)]
pub(super) struct Known {
    pub(super) lines: RefCell<Vec<Height>>,
    layout: RefCell<Option<Layout>>,
    /// Whether a tag changes the size of what it covers, by tag, as asked so far.
    sizes: RefCell<HashMap<usize, bool>>,
}

impl Known {
    /// Get the lines ready for a pass over `view`, whose text the rows say starts each line at
    /// `starts` (see `line_starts`), and say whether they can be used: not where the buffer counts
    /// lines the rows do not, breaking a line at U+2029 too, nor where the rows are for a text the
    /// buffer no longer holds — a keystroke the host has not laid yet, after which the rows'
    /// offsets measure the wrong lines. Every line is pending where the layout has changed since
    /// the last pass, and lines that are not the buffer's — an edit missed — are measured afresh.
    pub(super) fn ready(
        &self,
        view: &sourceview5::View,
        buffer: &sourceview5::Buffer,
        starts: &[i32],
    ) -> bool {
        let count = buffer.line_count() as usize;
        let mut known = self.lines.borrow_mut();
        if known.len() != count {
            *known = vec![Height::Unknown; count];
        }
        let layout = Some(Layout::of(view));
        if *self.layout.borrow() != layout {
            known.iter_mut().for_each(|h| *h = h.pending());
            *self.layout.borrow_mut() = layout;
        }
        count + 1 == starts.len() && starts.last() == Some(&buffer.char_count())
    }

    /// Line `line` has to be confirmed again.
    pub(super) fn touch(&self, line: usize) {
        if let Some(h) = self.lines.borrow_mut().get_mut(line) {
            *h = h.pending();
        }
    }

    /// Every line has to be confirmed again.
    fn all_pending(&self) {
        let mut lines = self.lines.borrow_mut();
        lines.iter_mut().for_each(|h| *h = h.pending());
    }

    /// Lines went in, the buffer counting `count` now, and the last of them is `last`.
    fn inserted(&self, last: usize, count: usize) {
        let mut lines = self.lines.borrow_mut();
        match count.checked_sub(lines.len()) {
            Some(added) if !lines.is_empty() && last >= added => {
                splice(&mut lines, last - added, 1, added + 1)
            }
            _ => lines.clear(),
        }
    }

    /// Lines from `line` on were joined into it, the buffer counting `count` now.
    fn deleted(&self, line: usize, count: usize) {
        let mut lines = self.lines.borrow_mut();
        match lines.len().checked_sub(count) {
            Some(removed) if line < lines.len() => splice(&mut lines, line, removed + 1, 1),
            _ => lines.clear(),
        }
    }

    /// `tag` went on or came off `start..end`.
    fn retagged(&self, tag: &gtk::TextTag, start: &gtk::TextIter, end: &gtk::TextIter) {
        let key = tag.as_ptr() as usize;
        let sizes = *self
            .sizes
            .borrow_mut()
            .entry(key)
            .or_insert_with(|| sizes_text(tag));
        if sizes {
            let mut lines = self.lines.borrow_mut();
            let end = (end.line() as usize + 1).min(lines.len());
            for h in &mut lines[(start.line() as usize).min(end)..end] {
                *h = h.pending();
            }
        }
    }
}

/// Whether `tag` sets anything that changes the size of the text it covers, other than the
/// comparison's own padding and hidden runs. An underline does not: Pango leaves a line's extents
/// alone for it, though GTK lays the line out again.
fn sizes_text(tag: &gtk::TextTag) -> bool {
    if is_pad(tag) || tag.name().as_deref() == Some(TAG_GAP) {
        return false;
    }
    tag.is_family_set()
        || tag.is_size_set()
        || tag.is_scale_set()
        || tag.is_style_set()
        || tag.is_variant_set()
        || tag.is_weight_set()
        || tag.is_stretch_set()
        || tag.is_language_set()
        || tag.is_letter_spacing_set()
        || tag.is_font_features_set()
        || tag.is_text_transform_set()
        || tag.is_pixels_above_lines_set()
        || tag.is_pixels_below_lines_set()
        || tag.is_pixels_inside_wrap_set()
        || tag.is_line_height_set()
        || tag.is_left_margin_set()
        || tag.is_right_margin_set()
        || tag.is_indent_set()
        || tag.is_rise_set()
        || tag.is_tabs_set()
        || tag.is_invisible_set()
        || tag.wraps_mode_set()
        || tag.allows_breaks_set()
        || tag.is_insert_hyphens_set()
}

/// Keep `known` in step with `buffer`: its lines spliced as text goes in and out, and pending
/// where a tag that sizes text goes on or comes off, or such a tag changes. The handlers, for the
/// caller to disconnect.
pub(super) fn listen(
    buffer: &sourceview5::Buffer,
    known: &Rc<Known>,
) -> Vec<(glib::Object, glib::SignalHandlerId)> {
    let mut ids = Vec::new();
    // After the buffer's own handlers, which leave the iters where the text went in or came out
    // and the line count the new one.
    let k = known.clone();
    let id = buffer.connect_local("insert-text", true, move |args| {
        let buffer = args[0].get::<gtk::TextBuffer>().ok()?;
        let at = args[1].get::<gtk::TextIter>().ok()?;
        k.inserted(at.line() as usize, buffer.line_count() as usize);
        None
    });
    ids.push((buffer.clone().upcast(), id));
    let k = known.clone();
    let id = buffer.connect_local("delete-range", true, move |args| {
        let buffer = args[0].get::<gtk::TextBuffer>().ok()?;
        let at = args[1].get::<gtk::TextIter>().ok()?;
        k.deleted(at.line() as usize, buffer.line_count() as usize);
        None
    });
    ids.push((buffer.clone().upcast(), id));
    let k = known.clone();
    let id = buffer.connect_apply_tag(move |_, tag, start, end| k.retagged(tag, start, end));
    ids.push((buffer.clone().upcast(), id));
    let k = known.clone();
    let id = buffer.connect_remove_tag(move |_, tag, start, end| k.retagged(tag, start, end));
    ids.push((buffer.clone().upcast(), id));
    // A tag's properties changed: every line it covers, which is every line as far as is known.
    // GTK's word for it, since a size the change unset leaves no trace on the tag.
    let table = buffer.tag_table();
    let k = known.clone();
    let id = table.connect_tag_changed(move |_, _, size| {
        k.sizes.borrow_mut().clear();
        if size {
            k.all_pending();
        }
    });
    ids.push((table.clone().upcast(), id));
    // A tag taken out of the table comes off the text without `remove-tag`, and frees its address
    // for another.
    let k = known.clone();
    let id = table.connect_tag_removed(move |_, tag| {
        k.sizes.borrow_mut().clear();
        if sizes_text(tag) {
            k.all_pending();
        }
    });
    ids.push((table.upcast(), id));
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use Height::{Laid, Unknown};

    #[test]
    fn an_edit_leaves_the_lines_around_it_their_figures() {
        let mut lines = vec![Laid(1), Laid(2), Laid(3), Laid(4)];
        // Two lines typed into line 1: it and the two after it are new.
        splice(&mut lines, 1, 1, 3);
        assert_eq!(
            lines,
            [Laid(1), Unknown, Unknown, Unknown, Laid(3), Laid(4)]
        );
        // Lines 2 to 4 joined into one.
        splice(&mut lines, 2, 3, 1);
        assert_eq!(lines, [Laid(1), Unknown, Unknown, Laid(4)]);
        // The whole text replaced.
        splice(&mut lines, 0, 4, 2);
        assert_eq!(lines, [Unknown, Unknown]);
    }
}
