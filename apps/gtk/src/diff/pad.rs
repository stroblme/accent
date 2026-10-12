//! Paragraph padding that aligns comparison-column rows.

use adw::prelude::*;

/// Pixel-count padding tag prefix, allowing current paragraph padding
/// to be read from the buffer ([`carried`]).
const PAD_ABOVE: &str = "diff-pad-above-";
const PAD_BELOW: &str = "diff-pad-below-";
/// A paragraph GTK has not laid out since its text or its padding changed: see [`measure`].
pub(super) const UNMEASURED: &str = "diff-unmeasured";

/// Blank space above and below each row's line in one column, that keeps it in step with the
/// other column. See [`padding`].
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Pads {
    pub(super) above: Vec<i32>,
    pub(super) below: Vec<i32>,
    /// Whole peer height when this column has no line to carry padding,
    /// such as the old side of an added file.
    pub(super) rest: i32,
}

/// Compute per-side padding and shared row tops from natural heights and `extra` space.
///
/// `None` means no visible line. Shorter lines pad below; missing lines reserve the full row.
/// Place blank space below a preceding change to keep its tint contiguous, otherwise above
/// the next line. Trailing space goes below the last line or into `rest` on empty sides.
/// Pure arithmetic keeps alignment correctness testable without a display.
pub(super) fn padding(
    columns: &[Vec<Option<i32>>],
    extra: &[i32],
    changed: &[bool],
) -> (Vec<Pads>, Vec<i32>) {
    let n = extra.len();
    let mut pads: Vec<Pads> = (0..columns.len())
        .map(|_| Pads {
            above: vec![0; n],
            below: vec![0; n],
            rest: 0,
        })
        .collect();
    let mut tops = Vec::with_capacity(n);
    let (mut y, mut carry, mut last) = (
        0,
        vec![0; columns.len()],
        vec![None::<usize>; columns.len()],
    );
    for r in 0..n {
        tops.push(y);
        let h = columns.iter().map(|c| c[r].unwrap_or(0)).max().unwrap_or(0) + extra[r];
        y += h;
        for (s, side) in columns.iter().enumerate() {
            let Some(own) = side[r] else {
                carry[s] += h;
                continue;
            };
            match last[s] {
                Some(l) if changed[l] => pads[s].below[l] += carry[s],
                _ => pads[s].above[r] = carry[s],
            }
            (carry[s], last[s]) = (h - own, Some(r));
        }
    }
    for (s, pad) in pads.iter_mut().enumerate() {
        match last[s] {
            Some(l) => pad.below[l] += carry[s],
            None => pad.rest = carry[s],
        }
    }
    (pads, tops)
}

/// Measure a line's natural height, subtracting `padded`, and report whether estimated.
///
/// Use validated GTK heights when available. Lazy layout returns zero before validation;
/// measure character positions within the paragraph meanwhile. U+2029 multi-line paragraphs
/// also need this path because GTK's figure covers only their first line.
///
/// [`UNMEASURED`] marks edited or repadded lines whose cached GTK height still reflects old
/// text or padding. Estimate until agreement, or clear the mark on a `fresh` pass after
/// layout idles, preventing stale heights from repeatedly shifting padding.
pub(super) fn measure(
    view: &sourceview5::View,
    buffer: &sourceview5::Buffer,
    from: i32,
    to: i32,
    padded: i32,
    fresh: bool,
) -> (i32, bool) {
    let first = buffer.iter_at_offset(from);
    let last = buffer.iter_at_offset((to - 1).max(from));
    let (_, height) = view.line_yrange(&first);
    let laid = (first.line() == last.line() && height > 0).then_some(height - padded);
    let unmeasured = buffer
        .tag_table()
        .lookup(UNMEASURED)
        .filter(|tag| first.has_tag(tag));
    let trusted = unmeasured.is_none() || fresh;
    #[cfg(feature = "bench")]
    tally(match (laid, trusted) {
        (Some(_), true) => 0,
        _ if height == 0 => 1,
        (Some(_), false) => 2,
        _ => 3,
    });
    if let (Some(laid), true) = (laid, trusted) {
        if let Some(tag) = unmeasured {
            unmark(buffer, &tag, first, to);
        }
        return (laid, false);
    }
    let (a, b) = (view.iter_location(&first), view.iter_location(&last));
    let margins = view.pixels_above_lines() + view.pixels_below_lines();
    let own = b.y() + b.height() - a.y() + margins;
    match (laid, unmeasured) {
        (Some(laid), Some(tag)) if (laid - own).abs() <= 1 => {
            unmark(buffer, &tag, first, to);
            (laid, false)
        }
        _ => (own, true),
    }
}

/// Fallback line height in the view font, including spacing, until measured.
pub(super) fn one_line(view: &sourceview5::View) -> i32 {
    let (_, height) = view.create_pango_layout(Some("X")).pixel_size();
    height + view.pixels_above_lines() + view.pixels_below_lines()
}

/// Take [`UNMEASURED`] off the line starting at `first`, the newline before it included.
fn unmark(buffer: &sourceview5::Buffer, tag: &gtk::TextTag, first: gtk::TextIter, to: i32) {
    let mut start = first;
    start.backward_char();
    buffer.remove_tag(tag, &start, &buffer.iter_at_offset(to));
}

#[cfg(feature = "bench")]
thread_local! {
    /// Measurement-source counters: GTK or fallback layout for absent,
    /// [`UNMEASURED`] or U+2029-split lines. Drills read and reset them.
    pub(super) static MEASURED: std::cell::Cell<[u64; 4]> = const { std::cell::Cell::new([0; 4]) };
}

#[cfg(feature = "bench")]
fn tally(how: usize) {
    MEASURED.with(|counts| {
        let mut now = counts.get();
        now[how] += 1;
        counts.set(now);
    });
}

/// Mark lines touched by `from..to` [`UNMEASURED`], including the preceding
/// newline so insertion at a line start inherits the tag.
pub(super) fn unmeasured(
    buffer: &impl IsA<gtk::TextBuffer>,
    from: &gtk::TextIter,
    to: &gtk::TextIter,
) {
    let (mut start, mut end) = (*from, *to);
    start.set_line_offset(0);
    start.backward_char();
    end.forward_line();
    buffer.apply_tag_by_name(UNMEASURED, &start, &end);
}

/// Read paragraph padding above and below `at`, beyond the view's margins.
///
/// Buffer tags describe what GTK's cached height includes and move with edits. Remembered
/// row padding would mismeasure lines before GTK revalidates them.
pub(super) fn carried(view: &sourceview5::View, at: &gtk::TextIter) -> (i32, i32) {
    let (mut above, mut below) = (0, 0);
    // Lowest priority first, so where a paste has left two, the one GTK uses is read last.
    for tag in at.tags() {
        let Some(name) = tag.name() else { continue };
        if name.starts_with(PAD_ABOVE) {
            above = tag.pixels_above_lines() - view.pixels_above_lines();
        } else if name.starts_with(PAD_BELOW) {
            below = tag.pixels_below_lines() - view.pixels_below_lines();
        }
    }
    (above, below)
}

/// Read padding for ascending `starts` in one tag-toggle walk, avoiding
/// per-paragraph tag lookup costs on large comparisons.
pub(super) fn carried_all(
    view: &sourceview5::View,
    buffer: &sourceview5::Buffer,
    starts: &[i32],
) -> Vec<(i32, i32)> {
    let base = [view.pixels_above_lines(), view.pixels_below_lines()];
    let mut tags = Vec::new();
    buffer.tag_table().foreach(|tag| {
        if is_pad(tag) {
            tags.push(tag.clone());
        }
    });
    // Every stretch a padding tag covers: where it begins and ends, the tag's priority, whether
    // it pads below, and its pixels over the view's own.
    let mut spans = Vec::new();
    for tag in &tags {
        let below = tag.name().is_some_and(|name| name.starts_with(PAD_BELOW));
        let px = match below {
            true => tag.pixels_below_lines() - base[1],
            false => tag.pixels_above_lines() - base[0],
        };
        let mut at = buffer.start_iter();
        while at.has_tag(tag) || at.forward_to_tag_toggle(Some(tag)) {
            let from = at.offset();
            at.forward_to_tag_toggle(Some(tag));
            spans.push((from, at.offset(), tag.priority(), below, px));
        }
    }
    spans.sort_unstable_by_key(|span| span.0);
    let (mut next, mut open) = (0, Vec::new());
    starts
        .iter()
        .map(|&at| {
            while let Some(&span) = spans.get(next).filter(|span| span.0 <= at) {
                open.push(span);
                next += 1;
            }
            open.retain(|span| span.1 > at);
            // The one GTK uses where two cover it, as `carried` reads it.
            let pick = |below: bool| {
                (open.iter().filter(|span| span.3 == below))
                    .max_by_key(|span| span.2)
                    .map_or(0, |span| span.4)
            };
            (pick(false), pick(true))
        })
        .collect()
}

/// Set paragraph padding to `above` and `below`; return whether it changed.
///
/// GTK reads spacing from the first character. Tags start at the preceding newline and end
/// before the paragraph's own newline so insertions and first-character deletion retain
/// padding without affecting the next line. First paragraphs and paragraphs below blank
/// lines need [`reclaim`] because the preceding newline is unavailable.
///
/// Do not tag a blank line's sole newline with its successor's padding: stale layout would
/// subtract that padding twice and grow alignment until GTK's height overflowed.
pub(super) fn pad(
    view: &sourceview5::View,
    buffer: &sourceview5::Buffer,
    from: i32,
    to: i32,
    above: i32,
    below: i32,
) -> bool {
    let first = buffer.iter_at_offset(from);
    let mut start = first;
    if start.backward_char() && start.starts_line() {
        start = first;
    }
    let end = buffer.iter_at_offset((to - 1).max(from + 1));
    let (had_above, had_below) = carried(view, &first);
    let mut changed = false;
    for (px, had, prefix, base) in [
        (above, had_above, PAD_ABOVE, view.pixels_above_lines()),
        (below, had_below, PAD_BELOW, view.pixels_below_lines()),
    ] {
        if px == had {
            continue;
        }
        changed = true;
        for tag in first.tags() {
            if tag.name().is_some_and(|name| name.starts_with(prefix)) {
                buffer.remove_tag(&tag, &start, &end);
            }
        }
        if px > 0 {
            buffer.apply_tag(&pad_tag(buffer, prefix, base + px), &start, &end);
        }
    }
    if changed {
        unmeasured(buffer, &first, &first);
    }
    changed
}

pub(super) fn is_pad(tag: &gtk::TextTag) -> bool {
    tag.name()
        .is_some_and(|name| name.starts_with(PAD_ABOVE) || name.starts_with(PAD_BELOW))
}

/// Lines whose padding an edit may displace: the caret line and first
/// line, which has no preceding newline to carry the tag.
pub(super) fn reclaim(buffer: &sourceview5::Buffer) {
    reclaim_line(buffer, &buffer.start_iter());
    reclaim_line(buffer, &buffer.iter_at_mark(&buffer.get_insert()));
}

/// Restore padding over text inserted at the start of `line`.
///
/// First lines and lines below blanks lack a preceding newline for [`pad`] to tag, so new
/// text can fall outside padding. Extend only tags ending with this line; other tags may
/// belong to a joined or succeeding paragraph and must not be reclaimed.
fn reclaim_line(buffer: &sourceview5::Buffer, line: &gtk::TextIter) {
    let mut start = *line;
    start.set_line_offset(0);
    let mut above = start;
    if (above.backward_char() && !above.starts_line()) || start.tags().iter().any(is_pad) {
        return;
    }
    // The start of the next line, which is where a tag of this one's ends at the latest.
    let mut to = start;
    to.forward_line();
    let mut at = start;
    while at.forward_to_tag_toggle(None::<&gtk::TextTag>) && at < to {
        let moved: Vec<gtk::TextTag> = at
            .toggled_tags(true)
            .into_iter()
            .filter(|tag| is_pad(tag) && ends_by(&at, tag, &to))
            .collect();
        if !moved.is_empty() {
            for tag in &moved {
                buffer.apply_tag(tag, &start, &at);
            }
            return;
        }
    }
}

/// Whether `tag`, which begins at `at`, is over by `to`.
fn ends_by(at: &gtk::TextIter, tag: &gtk::TextTag, to: &gtk::TextIter) -> bool {
    let mut end = *at;
    end.forward_to_tag_toggle(Some(tag));
    end <= *to
}

/// Create a `prefix`+`px` padding tag. `px` includes the base margin
/// because `pixels-above-lines` replaces rather than adds to view defaults.
fn pad_tag(buffer: &sourceview5::Buffer, prefix: &str, px: i32) -> gtk::TextTag {
    let name = format!("{prefix}{px}");
    let table = buffer.tag_table();
    table.lookup(&name).unwrap_or_else(|| {
        let tag = gtk::TextTag::new(Some(&name));
        match prefix {
            PAD_BELOW => tag.set_pixels_below_lines(px),
            _ => tag.set_pixels_above_lines(px),
        }
        table.add(&tag);
        tag
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_keeps_every_row_level_and_hands_a_fillers_share_on() {
        // Row 1 is a two-line paragraph on the old side facing one line; row 2 is a filler on
        // the old side; row 3 exists on both. Rows 1 and 2 are one change.
        let old = vec![Some(10), Some(20), None, Some(10)];
        let new = vec![Some(10), Some(10), Some(10), Some(10)];
        let changed = [false, true, true, false];
        let (pads, tops) = padding(&[old, new], &[0; 4], &changed);
        assert_eq!(tops, vec![0, 10, 30, 40]);
        assert_eq!(pads[0].above, vec![0; 4]);
        assert_eq!(
            pads[0].below,
            vec![0, 10, 0, 0],
            "the filler's row goes under the change it belongs to, and takes its tint"
        );
        assert_eq!(pads[1].above, vec![0; 4]);
        assert_eq!(
            pads[1].below,
            vec![0, 10, 0, 0],
            "the shorter line of row 1 starts with its partner, and the blank under it is its own"
        );

        // A deletion with no line of its own on the new side: the blank has no changed line to
        // go under, so it waits above the next one.
        let (pads, _) = padding(
            &[
                vec![Some(10), Some(10), Some(10)],
                vec![Some(10), None, Some(10)],
            ],
            &[0; 3],
            &[false, true, false],
        );
        assert_eq!(
            (pads[1].above.clone(), pads[1].below.clone()),
            (vec![0, 0, 10], vec![0; 3])
        );
    }

    #[test]
    fn trailing_fillers_and_gap_space_go_below_the_last_line() {
        let old = vec![Some(10), None, None];
        let new = vec![Some(10), Some(10), Some(10)];
        let (pads, _) = padding(&[old, new], &[0, 0, 0], &[false, true, true]);
        assert_eq!(pads[0].below, vec![20, 0, 0]);
        assert_eq!(pads[1].above, vec![0, 0, 0]);

        // A hidden run leaves the same blank on both sides, so the alignment is unmoved.
        let (pads, tops) = padding(
            &[
                vec![Some(10), None, Some(10)],
                vec![Some(10), None, Some(10)],
            ],
            &[0, 5, 0],
            &[false; 3],
        );
        assert_eq!(tops, vec![0, 10, 15]);
        assert_eq!(pads[0].above, vec![0, 0, 5]);
        assert_eq!(pads[1].above, vec![0, 0, 5]);
    }

    #[test]
    fn a_side_with_no_line_at_all_leaves_the_whole_column_under_its_text() {
        // A file a commit added: nothing on the old side to pad.
        let (pads, _) = padding(
            &[vec![None, None], vec![Some(20), Some(30)]],
            &[0, 0],
            &[true, true],
        );
        assert_eq!((pads[0].rest, pads[1].rest), (50, 0));
        assert_eq!(pads[0].below, vec![0, 0]);
    }

    #[test]
    fn three_columns_level_on_the_tallest() {
        // Row 1: the middle column's line is the tallest; the left has none there.
        let (pads, tops) = padding(
            &[
                vec![Some(10), None, Some(10)],
                vec![Some(10), Some(30), Some(10)],
                vec![Some(10), Some(20), Some(10)],
            ],
            &[0; 3],
            &[false, true, false],
        );
        assert_eq!(tops, vec![0, 10, 40]);
        assert_eq!(
            pads[0].above,
            vec![0, 0, 30],
            "the left's blank waits above its next line"
        );
        assert_eq!(pads[1].below, vec![0; 3]);
        assert_eq!(
            pads[2].below,
            vec![0, 10, 0],
            "the right's shorter change takes the rest"
        );
    }
}
