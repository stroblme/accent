//! Line diff of two texts.
//!
//! Deliberately about two strings and nothing else: the conflict view of Phase 1 and the git
//! views of Phase 4 are the same widget over the same rows.

use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};
use std::ops::{Range, RangeInclusive};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    Equal,
    Delete,
    Insert,
}

impl From<ChangeTag> for Op {
    fn from(tag: ChangeTag) -> Op {
        match tag {
            ChangeTag::Equal => Op::Equal,
            ChangeTag::Delete => Op::Delete,
            ChangeTag::Insert => Op::Insert,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiffLine {
    pub op: Op,
    /// 1-based line in the old text, `None` for an insertion.
    pub old_line: Option<usize>,
    /// 1-based line in the new text, `None` for a deletion.
    pub new_line: Option<usize>,
    /// The line without its trailing newline.
    pub text: String,
    /// Byte ranges into `text` that differ from the line this one is paired with. Empty for an
    /// unchanged line, and for a change the word diff judged too dissimilar to refine.
    pub emphasis: Vec<Range<usize>>,
}

/// Line diff of two texts, oldest-first, suitable for a side-by-side view.
///
/// ponytail: the word-level refinement runs on `similar`'s defaults — a 0.5 similarity floor,
/// below which a paired line is left unrefined, and a 500 ms deadline per hunk. Both are the
/// library's choices, not measurements of ours; `iter_inline_changes_with_options` (and the
/// `unicode` feature, for grapheme-accurate tokens) is the upgrade path if either shows.
pub fn lines(old: &str, new: &str) -> Vec<DiffLine> {
    let diff = TextDiff::from_lines(old, new);
    diff.ops()
        .iter()
        .flat_map(|op| diff.iter_inline_changes(op))
        .map(|c| {
            let (mut text, mut emphasis) = (String::new(), Vec::new());
            for (emphasized, value) in c.iter_strings_lossy() {
                let start = text.len();
                text.push_str(&value);
                if emphasized {
                    emphasis.push(start..text.len());
                }
            }
            // `similar` never emphasises a newline, so dropping the line ending afterwards cannot
            // cut a range short.
            text.truncate(text.trim_end_matches(['\r', '\n']).len());
            DiffLine {
                op: c.tag().into(),
                old_line: c.old_index().map(|i| i + 1),
                new_line: c.new_index().map(|i| i + 1),
                text,
                emphasis,
            }
        })
        .collect()
}

/// Line diff of two texts without the word-level refinement [`lines`] does: the same rows in the
/// same order, every `emphasis` empty.
///
/// What a gutter needs — which lines changed, and how — for a fraction of the cost:
/// `iter_inline_changes` re-diffs each hunk word by word under a 500 ms budget, and a change bar
/// three pixels wide has nowhere to put the answer.
pub fn line_ops(old: &str, new: &str) -> Vec<DiffLine> {
    let mut lines = changes(old, new);
    for line in &mut lines {
        line.text
            .truncate(line.text.trim_end_matches(['\r', '\n']).len());
    }
    lines
}

/// [`line_ops`] with each line's text as its own text has it, line ending included: what
/// [`apply_lines`] puts back together.
fn changes(old: &str, new: &str) -> Vec<DiffLine> {
    TextDiff::from_lines(old, new)
        .iter_all_changes()
        .map(|c| DiffLine {
            op: c.tag().into(),
            old_line: c.old_index().map(|i| i + 1),
            new_line: c.new_index().map(|i| i + 1),
            text: c.value().to_string(),
            emphasis: Vec::new(),
        })
        .collect()
}

/// One row of a side-by-side view: indices into the `DiffLine` list, `None` where a side has no
/// line and shows a filler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub old: Option<usize>,
    pub new: Option<usize>,
}

/// Which of the two texts: `Old` is the left column of a side-by-side view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Old,
    New,
}

impl Side {
    pub fn other(self) -> Side {
        match self {
            Side::Old => Side::New,
            Side::New => Side::Old,
        }
    }

    /// Where this side sits in an `[old, new]` pair.
    pub fn idx(self) -> usize {
        match self {
            Side::Old => 0,
            Side::New => 1,
        }
    }

    /// The index into the diff of the line this row shows on this side.
    pub fn of(self, row: &Row) -> Option<usize> {
        match self {
            Side::Old => row.old,
            Side::New => row.new,
        }
    }

    /// The 1-based line number of `line` in this side's text.
    pub fn number(self, line: &DiffLine) -> Option<usize> {
        match self {
            Side::Old => line.old_line,
            Side::New => line.new_line,
        }
    }
}

/// Turn the flat diff into rows: an `Equal` line sits on both sides, a run of `Delete`s is paired
/// row by row with the `Insert` run beside it, and whichever run is shorter gets `None` fillers so
/// the two columns stay in step.
///
/// This is the whole correctness surface of a side-by-side widget, so it lives here as a plain
/// function over plain data rather than inside a toolkit.
pub fn align(lines: &[DiffLine]) -> Vec<Row> {
    let mut rows = Vec::new();
    let (mut dels, mut ins) = (Vec::new(), Vec::new());
    for (i, line) in lines.iter().enumerate() {
        match line.op {
            Op::Equal => {
                flush(&mut dels, &mut ins, &mut rows);
                rows.push(Row {
                    old: Some(i),
                    new: Some(i),
                });
            }
            // A delete after an insert starts a new pairing: `similar` emits deletes before
            // inserts within a hunk, so this only guards against input that does not.
            Op::Delete => {
                if !ins.is_empty() {
                    flush(&mut dels, &mut ins, &mut rows);
                }
                dels.push(i);
            }
            Op::Insert => ins.push(i),
        }
    }
    flush(&mut dels, &mut ins, &mut rows);
    rows
}

fn flush(dels: &mut Vec<usize>, ins: &mut Vec<usize>, rows: &mut Vec<Row>) {
    for i in 0..dels.len().max(ins.len()) {
        rows.push(Row {
            old: dels.get(i).copied(),
            new: ins.get(i).copied(),
        });
    }
    dels.clear();
    ins.clear();
}

/// Whether a row shows the same unchanged line on both sides.
fn is_equal(lines: &[DiffLine], row: &Row) -> bool {
    match (row.old, row.new) {
        (Some(old), Some(new)) => old == new && lines.get(old).is_some_and(|l| l.op == Op::Equal),
        _ => false,
    }
}

/// The maximal runs of rows that are not unchanged on both sides, as ranges into `rows`.
pub fn hunks(lines: &[DiffLine], rows: &[Row]) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        if is_equal(lines, row) {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.end == i => last.end = i + 1,
            _ => out.push(i..i + 1),
        }
    }
    out
}

/// The rows a changes-only view hides, as ranges into `rows`: non-empty, sorted and
/// non-overlapping.
///
/// A run of unchanged rows between two hunks keeps `context` rows at each end, so it is hidden
/// only where it is longer than both margins together. The runs at the start and end of the file
/// have a hunk on one side only and keep one margin. A file with no hunks at all has nothing to
/// show, so all of it is one gap.
pub fn gaps(lines: &[DiffLine], rows: &[Row], context: usize) -> Vec<Range<usize>> {
    let hunks = hunks(lines, rows);
    let (Some(first), Some(last)) = (hunks.first(), hunks.last()) else {
        // Nothing changed, so a changes-only view shows nothing and hides all of it.
        let whole = 0..rows.len();
        return if rows.is_empty() {
            Vec::new()
        } else {
            vec![whole]
        };
    };
    let mut out = Vec::new();
    if first.start > context {
        out.push(0..first.start - context);
    }
    for pair in hunks.windows(2) {
        let (start, end) = (pair[0].end, pair[1].start);
        if end - start > 2 * context {
            out.push(start + context..end - context);
        }
    }
    if rows.len() - last.end > context {
        out.push(last.end + context..rows.len());
    }
    out
}

/// `old` with the changes `new` makes on `side`'s lines `picked` (1-based, inclusive), and no
/// others: Stage Selected Lines, `old` being the index and `new` the working tree. Both texts
/// have `\n` line endings, as a buffer holds them.
///
/// A selection is read the way the side-by-side view shows it ([`align`]): it covers the rows
/// its first and last lines are on and every row between, so a changed line goes together with
/// the line beside it. A row with no line on `side` — lines deleted, seen from the new side —
/// cannot be selected there, so it goes with the line above it: a rewrite selected whole takes
/// the lines it lost as well, and a deletion is taken with the line before the gap it left.
pub fn apply_lines(old: &str, new: &str, side: Side, picked: RangeInclusive<usize>) -> String {
    mix(old, new, side, &picked, true)
}

/// `new` with the changes it makes on `side`'s lines `picked` undone, and no others: Unstage
/// Selected Lines, `old` being HEAD and `new` the index. The selection is read as
/// [`apply_lines`] reads it.
pub fn revert_lines(old: &str, new: &str, side: Side, picked: RangeInclusive<usize>) -> String {
    mix(old, new, side, &picked, false)
}

/// Put a text back together row by row: a row the selection covers gives its new side's line
/// when `apply`ing and its old side's when reverting, and every other row the opposite. An
/// unchanged row is the same line on both.
fn mix(old: &str, new: &str, side: Side, picked: &RangeInclusive<usize>, apply: bool) -> String {
    let lines = changes(old, new);
    let rows = align(&lines);
    let covered = covered(&lines, &rows, side, picked);
    let mut out = String::with_capacity(old.len().max(new.len()));
    for (r, row) in rows.iter().enumerate() {
        let from = match covered.contains(&r) == apply {
            true => Side::New,
            false => Side::Old,
        };
        let Some(i) = from.of(row) else {
            continue;
        };
        // A last line with no newline is last no longer.
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&lines[i].text);
    }
    out
}

/// The rows a selection of `side`'s lines `picked` covers, as a range into `rows`: see
/// [`apply_lines`].
fn covered(
    lines: &[DiffLine],
    rows: &[Row],
    side: Side,
    picked: &RangeInclusive<usize>,
) -> Range<usize> {
    let selected = |row: &Row| {
        side.of(row)
            .and_then(|i| side.number(&lines[i]))
            .is_some_and(|n| picked.contains(&n))
    };
    let (Some(first), Some(last)) = (
        rows.iter().position(selected),
        rows.iter().rposition(selected),
    ) else {
        return 0..0;
    };
    let end = rows[last + 1..]
        .iter()
        .position(|row| side.of(row).is_some())
        .map_or(rows.len(), |n| last + 1 + n);
    first..end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(
        op: Op,
        old: Option<usize>,
        new: Option<usize>,
        text: &str,
        emphasis: Option<Range<usize>>,
    ) -> DiffLine {
        DiffLine {
            op,
            old_line: old,
            new_line: new,
            text: text.to_string(),
            emphasis: emphasis.into_iter().collect(),
        }
    }

    #[test]
    fn line_diff_marks_equal_delete_and_insert() {
        let d = lines("alpha\nbravo\ncharlie\n", "alpha\nbravo two\ncharlie\n");
        assert_eq!(
            d,
            vec![
                line(Op::Equal, Some(1), Some(1), "alpha", None),
                line(Op::Delete, Some(2), None, "bravo", None),
                line(Op::Insert, None, Some(2), "bravo two", Some(5..9)),
                line(Op::Equal, Some(3), Some(3), "charlie", None),
            ]
        );
    }

    #[test]
    fn line_ops_is_the_same_diff_without_the_word_level_pass() {
        let (old, new) = ("alpha\nbravo\ncharlie\n", "alpha\nbravo two\ncharlie\n");
        let refined = lines(old, new);
        let plain = line_ops(old, new);
        assert!(plain.iter().all(|l| l.emphasis.is_empty()));
        assert_eq!(
            plain,
            refined
                .into_iter()
                .map(|l| DiffLine {
                    emphasis: Vec::new(),
                    ..l
                })
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn word_diff_emphasises_only_what_changed() {
        let d = lines("a b c\n", "a x c\n");
        assert_eq!(
            d,
            vec![
                line(Op::Delete, Some(1), None, "a b c", Some(2..3)),
                line(Op::Insert, None, Some(1), "a x c", Some(2..3)),
            ]
        );
        assert!(lines("same\n", "same\n")[0].emphasis.is_empty());
    }

    fn texts<'a>(
        d: &'a [DiffLine],
        rows: &[Row],
        side: fn(&Row) -> Option<usize>,
    ) -> Vec<Option<&'a str>> {
        rows.iter()
            .map(|r| side(r).map(|i| d[i].text.as_str()))
            .collect()
    }

    /// A one-gap expectation, spelled without a `[a..b]` literal so clippy does not read it as a
    /// range that was meant to be collected.
    fn one(range: Range<usize>) -> Vec<Range<usize>> {
        vec![range]
    }

    fn old(r: &Row) -> Option<usize> {
        r.old
    }

    fn new(r: &Row) -> Option<usize> {
        r.new
    }

    #[test]
    fn alignment_pairs_equal_lines_and_pads_changes() {
        let d = lines("alpha\nbravo\ncharlie\n", "alpha\nbravo two\ncharlie\n");
        let rows = align(&d);
        assert_eq!(rows.len(), 3, "one row per line, the change paired up");
        assert_eq!(
            texts(&d, &rows, old),
            vec![Some("alpha"), Some("bravo"), Some("charlie")]
        );
        assert_eq!(
            texts(&d, &rows, new),
            vec![Some("alpha"), Some("bravo two"), Some("charlie")]
        );
    }

    #[test]
    fn alignment_handles_pure_insert_and_pure_delete() {
        let d = lines("alpha\n", "alpha\nbravo\ncharlie\n");
        let rows = align(&d);
        assert_eq!(texts(&d, &rows, old), vec![Some("alpha"), None, None]);
        assert_eq!(
            texts(&d, &rows, new),
            vec![Some("alpha"), Some("bravo"), Some("charlie")]
        );

        let d = lines("alpha\nbravo\ncharlie\n", "alpha\n");
        let rows = align(&d);
        assert_eq!(
            texts(&d, &rows, old),
            vec![Some("alpha"), Some("bravo"), Some("charlie")]
        );
        assert_eq!(texts(&d, &rows, new), vec![Some("alpha"), None, None]);
    }

    #[test]
    fn alignment_keeps_source_line_numbers() {
        let d = lines("alpha\nbravo\ncharlie\n", "alpha\nx\ny\nbravo\ncharlie\n");
        let rows = align(&d);
        let numbers = |side: fn(&Row) -> Option<usize>, pick: fn(&DiffLine) -> Option<usize>| {
            rows.iter()
                .map(|r| side(r).and_then(|i| pick(&d[i])))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            numbers(old, |l| l.old_line),
            vec![Some(1), None, None, Some(2), Some(3)],
            "filler rows carry no number and the rest keep their old-text line"
        );
        assert_eq!(
            numbers(new, |l| l.new_line),
            vec![Some(1), Some(2), Some(3), Some(4), Some(5)]
        );
    }

    #[test]
    fn hunks_are_the_runs_of_rows_that_changed() {
        let d = lines("a\nb\nc\nd\n", "a\nB\nc\nD\nE\n");
        let rows = align(&d);
        assert_eq!(rows.len(), 5);
        assert_eq!(hunks(&d, &rows), vec![1..2, 3..5]);
    }

    /// Ten unchanged lines with one of them rewritten: four rows lead up to the change and five
    /// follow it, so three rows of context leaves one row hidden above and two below.
    #[test]
    fn a_lone_change_hides_the_run_that_outgrows_its_context() {
        let old_text: String = (1..=10).map(|i| format!("l{i}\n")).collect();
        let new_text: String = (1..=10)
            .map(|i| {
                if i == 5 {
                    "L5\n".to_string()
                } else {
                    format!("l{i}\n")
                }
            })
            .collect();
        let d = lines(&old_text, &new_text);
        let rows = align(&d);
        assert_eq!(hunks(&d, &rows), vec![4..5]);
        assert_eq!(gaps(&d, &rows, 3), vec![0..1, 8..10]);
    }

    #[test]
    fn a_long_run_between_two_changes_keeps_a_margin_at_each_end() {
        let middle: String = (1..=20).map(|i| format!("e{i}\n")).collect();
        let d = lines(
            &format!("first\n{middle}last\n"),
            &format!("FIRST\n{middle}LAST\n"),
        );
        let rows = align(&d);
        assert_eq!(hunks(&d, &rows), vec![0..1, 21..22]);
        assert_eq!(gaps(&d, &rows, 3), one(4..18), "20 rows less two margins");
    }

    #[test]
    fn identical_texts_are_one_gap_and_no_context_hides_every_equal_row() {
        let same = "a\nb\nc\n";
        let d = lines(same, same);
        let rows = align(&d);
        assert!(hunks(&d, &rows).is_empty());
        assert_eq!(gaps(&d, &rows, 3), one(0..3));

        let d = lines("a\nb\nc\nd\n", "a\nB\nc\nD\nE\n");
        let rows = align(&d);
        assert_eq!(gaps(&d, &rows, 0), vec![0..1, 2..3]);
    }

    #[test]
    fn applying_lines_takes_the_added_ones_selected_and_no_others() {
        assert_eq!(
            apply_lines("a\nb\n", "a\nx\ny\nb\n", Side::New, 3..=3),
            "a\ny\nb\n"
        );
    }

    #[test]
    fn applying_lines_takes_a_deletion_from_either_side() {
        let (old, new) = ("a\nb\nc\nd\n", "a\nd\n");
        assert_eq!(
            apply_lines(old, new, Side::Old, 2..=2),
            "a\nc\nd\n",
            "b selected where it still is"
        );
        assert_eq!(
            apply_lines(old, new, Side::New, 1..=1),
            "a\nd\n",
            "where the new side has no line, the deletion goes with the line above it"
        );
    }

    #[test]
    fn a_rewrite_partly_selected_takes_the_rows_the_selection_is_on() {
        let (old, new) = ("a\nb\nc\nd\n", "a\nB\nC\nX\nd\n");
        assert_eq!(apply_lines(old, new, Side::New, 2..=2), "a\nB\nc\nd\n");
        assert_eq!(
            apply_lines("a\nb\nc\nd\n", "a\nB\nd\n", Side::New, 2..=2),
            "a\nB\nd\n",
            "a paragraph rewritten shorter, selected whole, is taken whole"
        );
    }

    #[test]
    fn a_selection_across_two_hunks_takes_both_and_leaves_the_third() {
        let (old, new) = ("1\n2\n3\n4\n5\n6\n7\n", "1\nA\n3\nB\n5\nC\n7\n");
        assert_eq!(
            apply_lines(old, new, Side::New, 2..=4),
            "1\nA\n3\nB\n5\n6\n7\n"
        );
    }

    #[test]
    fn a_last_line_without_a_newline_gets_one_only_when_something_follows_it() {
        assert_eq!(apply_lines("a\nb", "a\nb\nc", Side::New, 3..=3), "a\nb\nc");
        assert_eq!(apply_lines("a\nb\n", "a\nB", Side::New, 2..=2), "a\nB");
        assert_eq!(
            apply_lines("a\nb\nc\n", "a\nX", Side::Old, 2..=2),
            "a\nX\nc\n",
            "the c left alone follows what is now the last line"
        );
    }

    #[test]
    fn reverting_lines_undoes_the_selected_changes_and_keeps_the_rest() {
        assert_eq!(
            revert_lines("a\nb\nc\n", "a\nB\nc\nd\n", Side::New, 2..=2),
            "a\nb\nc\nd\n"
        );
    }
}
