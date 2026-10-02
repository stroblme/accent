//! The blank space that keeps the two columns level: how much each row needs, and the
//! padding tags that lay it on a paragraph.

use adw::prelude::*;

use super::{ADDED_HUE, Band, REMOVED_HUE, Side};

/// The blank space above a paragraph, and below the last, that keeps the two columns level: one
/// tag per pixel count, named this plus the count, so a paragraph's padding can be read back off
/// the buffer. See [`carried`].
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
    /// What no line of this column can carry because it has none at all — a file a commit added,
    /// seen from before it — which is the whole of the other column's height.
    pub(super) rest: i32,
}

/// From the natural height of every row on each side — `None` where a side has no visible line
/// there — the space each side adds so that row `i` starts at the same height on both, and the
/// top of every row in that shared grid. `extra` is space both sides leave at a row on purpose,
/// which is where a hidden run's button goes.
///
/// A line shorter than its partner leaves the difference under it, so two paired lines start on
/// the same row; a row with no line on a side leaves all of it. The space goes below the side's
/// line before it where that line is a change (`changed`, per row), so a change and the blank
/// that levels it are one tinted block, and above the side's next line otherwise; what is left
/// after the last line goes below it, or to `rest` on a side with no line. This is the whole
/// correctness surface of the alignment, so it is a plain function over plain numbers.
pub(super) fn padding(
    old: &[Option<i32>],
    new: &[Option<i32>],
    extra: &[i32],
    changed: &[bool],
) -> ([Pads; 2], Vec<i32>) {
    let n = old.len();
    let mut pads = [(); 2].map(|_| Pads {
        above: vec![0; n],
        below: vec![0; n],
        rest: 0,
    });
    let mut tops = Vec::with_capacity(n);
    let (mut y, mut carry, mut last) = (0, [0, 0], [None::<usize>; 2]);
    for r in 0..n {
        tops.push(y);
        let h = old[r].unwrap_or(0).max(new[r].unwrap_or(0)) + extra[r];
        y += h;
        for (s, side) in [old, new].into_iter().enumerate() {
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
    for s in 0..2 {
        match last[s] {
            Some(l) => pads[s].below[l] += carry[s],
            None => pads[s].rest = carry[s],
        }
    }
    (pads, tops)
}

/// Where `side` has no line in a hunk at all — the other side only adds, or only deletes — the
/// blank that levels it, as `(y, height, hue)` rows for `multicaret::View::set_bands`, in the hue
/// of the lines it faces. A run of rows it has no line in that touches a changed line of its own
/// is part of a change already, and that line's tint covers it (see [`padding`]).
pub(super) fn bands(
    heights: &[Vec<Option<i32>>; 2],
    extra: &[i32],
    changed: &[bool],
    tops: &[i32],
    side: Side,
) -> Vec<Band> {
    let (n, own) = (changed.len(), &heights[side.idx()]);
    let hue = match side {
        Side::Old => ADDED_HUE,
        Side::New => REMOVED_HUE,
    };
    let bottom =
        |r: usize| tops[r] + heights[0][r].unwrap_or(0).max(heights[1][r].unwrap_or(0)) + extra[r];
    let lacks = |r: usize| changed[r] && own[r].is_none();
    let mut out = Vec::new();
    let mut r = 0;
    while r < n {
        if !lacks(r) {
            r += 1;
            continue;
        }
        let start = r;
        while r < n && lacks(r) {
            r += 1;
        }
        if (start == 0 || !changed[start - 1]) && (r == n || !changed[r]) {
            out.push((tops[start], bottom(r - 1) - tops[start], hue));
        }
    }
    out
}

/// The natural height of one line of `buffer` as `view` lays it out, wrapping and all, with
/// `padded` — the pixels this module has put above and below it — taken back off. The flag
/// says the figure is an estimate.
///
/// GTK's own figure is used once it has one: a validated line's height is the paragraph's Pango
/// extent rounded once, margins on. GTK validates lazily, though, and reports 0 for a line it
/// has not reached, so until then the height is measured inside the paragraph — the first and
/// last character's positions come from the same layout, so their difference is right to
/// within the pixel a wrapped paragraph's per-line rounding can add — and the caller asks again
/// once GTK has caught up. A paragraph the buffer counts as several lines (U+2029) is measured
/// that way too, since GTK's figure would be for its first line alone.
///
/// So is one marked [`UNMEASURED`], until GTK's figure agrees with it: GTK keeps the height it
/// last laid a line out at until it lays it out again, so its figure for a line just edited is
/// the old text's, and one for a paragraph just re-padded, less the padding it carries now, is
/// off by the change. Read as it was, the relayout run on a keystroke itself padded the other
/// column for the old text, and a relayout run before GTK had laid out a re-padded paragraph
/// moved its padding by the change again on every pass.
pub(super) fn measure(
    view: &sourceview5::View,
    buffer: &sourceview5::Buffer,
    from: i32,
    to: i32,
    padded: i32,
) -> (i32, bool) {
    let first = buffer.iter_at_offset(from);
    let last = buffer.iter_at_offset((to - 1).max(from));
    let (_, height) = view.line_yrange(&first);
    let laid = (first.line() == last.line() && height > 0).then_some(height - padded);
    let unmeasured = buffer
        .tag_table()
        .lookup(UNMEASURED)
        .filter(|tag| first.has_tag(tag));
    if let (Some(laid), None) = (laid, &unmeasured) {
        return (laid, false);
    }
    let (a, b) = (view.iter_location(&first), view.iter_location(&last));
    let margins = view.pixels_above_lines() + view.pixels_below_lines();
    let own = b.y() + b.height() - a.y() + margins;
    match (laid, unmeasured) {
        (Some(laid), Some(tag)) if (laid - own).abs() <= 1 => {
            let mut start = first;
            start.backward_char();
            buffer.remove_tag(&tag, &start, &buffer.iter_at_offset(to));
            (laid, false)
        }
        _ => (own, true),
    }
}

/// Mark the lines `from..to` touches [`UNMEASURED`], from the newline before the first of them,
/// so that text typed at a line's start lands inside the mark: what a comparison does to an edit
/// on its way into the buffer, and to a paragraph it re-pads.
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

/// The padding the paragraph starting at `at` carries, above and below, over the view's own
/// margins.
///
/// Read off the buffer rather than remembered, because it is what GTK's figure for the line
/// includes: a line keeps the height it was last laid out at until GTK lays it out again, even
/// after an edit or a tag change, so taking off anything but the padding it really carries
/// measures it wrong. Rows renumber after an edit, but the tags move with the text.
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

/// Give the paragraph `from..to` (its newline included) `above` pixels of padding above it and
/// `below` under it, where its first character does not carry exactly that already: a paragraph
/// left alone is not laid out again. `true` when it was not left alone.
///
/// GTK reads a paragraph's spacing off its first character alone, but the tags cover the newline
/// before the paragraph and the paragraph itself up to its own newline. Text typed at its start
/// lands inside them — text inserted where a tag begins does not take the tag — and so does the
/// character left when the first is deleted, where tags on the first character alone were lost
/// to either edit and the line was laid out bare for a frame. The paragraph's own newline is left
/// out because a tag ending at the next line's start is taken by text typed there. The first
/// paragraph has no newline before it, and one under a blank line leaves that line's newline to
/// it: [`reclaim`] makes up for both.
///
/// A blank line is laid out with the spacing its one character carries, so a paragraph's tags on
/// it padded the blank line too, and a relayout run before GTK had laid it out again took both of
/// the paragraph's pads off the blank line's height and gave them to the paragraph as padding
/// above: twice the padding on every such pass, until a line's height overflowed GTK's `int`.
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

/// The lines a change can have pushed a padding tag out of: the one the caret is in, which is
/// where typing lands, and the first, which has no newline before it whoever wrote into it.
pub(super) fn reclaim(buffer: &sourceview5::Buffer) {
    reclaim_line(buffer, &buffer.start_iter());
    reclaim_line(buffer, &buffer.iter_at_mark(&buffer.get_insert()));
}

/// Put text typed at the start of `line` back under the paragraph's padding.
///
/// [`pad`] starts a paragraph's tags at the newline before it, so text typed at its start lands
/// inside them. Two paragraphs cannot both own that newline, though: the first line of all has
/// none, and the newline before a paragraph under a blank line is the blank line's only
/// character, which [`pad`] leaves to the blank line. There the tags begin at the
/// paragraph's own first character and a character typed ahead of them goes in outside: GTK lays
/// the line out bare for a frame, and the relayout, finding no padding on its first character,
/// measures it at the height it had last been laid out at, padding and all. Stretched back over
/// what was typed, the tags cover the first character again and say what GTK last used.
///
/// Only there: in any other line a tag beginning inside it is what is left of the paragraph an
/// edit joined onto it, and stretched back it gave the joined line that paragraph's padding until
/// the next relayout took it off again. The line's own tags are the ones that end with it: the
/// next paragraph's begins at this line's newline as well, and runs on past it.
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

/// The tag named `prefix` plus `px`, which sets that many pixels above or below a paragraph.
/// `pixels-above-lines` on a tag replaces the view's default rather than adding to it, so `px`
/// is the base margin plus the pad.
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
        let old = [Some(10), Some(20), None, Some(10)];
        let new = [Some(10), Some(10), Some(10), Some(10)];
        let changed = [false, true, true, false];
        let (pads, tops) = padding(&old, &new, &[0; 4], &changed);
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
            &[Some(10), Some(10), Some(10)],
            &[Some(10), None, Some(10)],
            &[0; 3],
            &[false, true, false],
        );
        assert_eq!(
            (pads[1].above.clone(), pads[1].below.clone()),
            (vec![0, 0, 10], vec![0; 3])
        );
    }

    #[test]
    fn a_hunk_with_no_line_on_one_side_leaves_a_band_there_in_the_other_sides_hue() {
        // Rows 1 and 2 only delete; the new side has nothing there.
        let heights = [
            vec![Some(10), Some(20), Some(10), Some(10)],
            vec![Some(10), None, None, Some(10)],
        ];
        let (extra, changed) = ([0; 4], [false, true, true, false]);
        let tops = [0, 10, 30, 40];
        assert_eq!(
            bands(&heights, &extra, &changed, &tops, Side::New),
            vec![(10, 30, REMOVED_HUE)]
        );
        assert_eq!(bands(&heights, &extra, &changed, &tops, Side::Old), vec![]);

        // A deletion under a changed pair is that change's blank, which its own tint covers.
        let heights = [
            vec![Some(10), Some(20), Some(10), Some(10)],
            vec![Some(10), Some(10), None, Some(10)],
        ];
        assert_eq!(bands(&heights, &extra, &changed, &tops, Side::New), vec![]);
    }

    #[test]
    fn trailing_fillers_and_gap_space_go_below_the_last_line() {
        let old = [Some(10), None, None];
        let new = [Some(10), Some(10), Some(10)];
        let (pads, _) = padding(&old, &new, &[0, 0, 0], &[false, true, true]);
        assert_eq!(pads[0].below, vec![20, 0, 0]);
        assert_eq!(pads[1].above, vec![0, 0, 0]);

        // A hidden run leaves the same blank on both sides, so the alignment is unmoved.
        let (pads, tops) = padding(
            &[Some(10), None, Some(10)],
            &[Some(10), None, Some(10)],
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
        let (pads, _) = padding(&[None, None], &[Some(20), Some(30)], &[0, 0], &[true, true]);
        assert_eq!((pads[0].rest, pads[1].rest), (50, 0));
        assert_eq!(pads[0].below, vec![0, 0]);
    }
}
