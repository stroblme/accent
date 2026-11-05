//! Line diffs shared by conflict and git views, independent of the UI toolkit.

use serde::{Deserialize, Serialize};
use similar::{ChangeTag, DiffTag, TextDiff};
use std::ops::{Range, RangeInclusive};
use std::time::{Duration, Instant};

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
    /// Byte ranges into `text` that differ from the line [`matches`] pairs this one with. Empty for
    /// an unchanged line, a change paired with none, and a change the word diff judged too
    /// dissimilar to refine.
    pub emphasis: Vec<Range<usize>>,
}

/// Total word-refinement budget for [`lines`]. Remaining pairs stay unrefined when it expires,
/// bounding UI latency; background callers can allow more time through [`lines_within`].
pub const REFINE: Duration = Duration::from_millis(100);

/// Line diff in source order for side-by-side views.
///
/// The changed lines [`align`] lays beside each other for their words ([`matches`]) receive
/// word-level emphasis; the rest, and pairs below `similar`'s 0.5 similarity floor, stay
/// unrefined.
pub fn lines(old: &str, new: &str) -> Vec<DiffLine> {
    lines_within(old, new, REFINE).0
}

/// [`lines`] with `refine` for the word-level pass, and whether every pair had its turn in it.
pub fn lines_within(old: &str, new: &str, refine: Duration) -> (Vec<DiffLine>, bool) {
    let diff = TextDiff::from_lines(old, new);
    let (deadline, mut whole) = (Instant::now() + refine, true);
    let mut lines = Vec::new();
    for op in diff.ops() {
        let at = lines.len();
        lines.extend(diff.iter_changes(op).map(|c| {
            let text = c.value();
            DiffLine {
                op: c.tag().into(),
                old_line: c.old_index().map(|i| i + 1),
                new_line: c.new_index().map(|i| i + 1),
                text: text.trim_end_matches(['\r', '\n']).to_string(),
                emphasis: Vec::new(),
            }
        }));
        // A replacement's deletions come first, then its insertions, matched as `align` matches
        // them.
        let (old_len, new_len) = (op.old_range().len(), op.new_range().len());
        if let DiffTag::Replace = op.tag() {
            let text = |i: usize| lines[i].text.as_str();
            let pairs = matches(
                &(at..at + old_len).map(text).collect::<Vec<_>>(),
                &(at + old_len..at + old_len + new_len)
                    .map(text)
                    .collect::<Vec<_>>(),
            );
            for (o, n) in pairs {
                if Instant::now() > deadline {
                    whole = false;
                    break;
                }
                let (o, n) = (at + o, at + old_len + n);
                [lines[o].emphasis, lines[n].emphasis] =
                    emphasis(&lines[o].text, &lines[n].text, deadline);
            }
        }
    }
    (lines, whole)
}

/// Where `old` and `new`, a changed line and its partner, differ word by word: byte ranges into
/// each, or none where they are too unlike for that to read.
fn emphasis(old: &str, new: &str, deadline: Instant) -> [Vec<Range<usize>>; 2] {
    let diff = TextDiff::from_lines(old, new);
    let mut emphasis = [Vec::new(), Vec::new()];
    for op in diff.ops() {
        for change in diff.iter_inline_changes_deadline(op, Some(deadline)) {
            let side = match change.tag() {
                ChangeTag::Delete => 0,
                ChangeTag::Insert => 1,
                ChangeTag::Equal => continue,
            };
            let mut at = 0;
            for (emphasized, value) in change.iter_strings_lossy() {
                if emphasized {
                    emphasis[side].push(at..at + value.len());
                }
                at += value.len();
            }
        }
    }
    emphasis
}

/// The same rows as [`lines`] with empty `emphasis`, avoiding word refinement for gutter change
/// bars.
pub fn line_ops(old: &str, new: &str) -> Vec<DiffLine> {
    let mut lines = changes(old, new);
    for line in &mut lines {
        line.text
            .truncate(line.text.trim_end_matches(['\r', '\n']).len());
    }
    lines
}

/// Ordered replacements turning `old` into `new`: zero-based old-line ranges with replacement
/// text, including line endings. Unchanged lines are retained so editor layouts survive a
/// reread.
pub fn replaced(old: &str, new: &str) -> Vec<(Range<usize>, String)> {
    let diff = TextDiff::from_lines(old, new);
    (diff.ops().iter())
        .filter(|op| op.tag() != DiffTag::Equal)
        .map(|op| {
            let text = op.new_range().filter_map(|i| diff.new_slice(i)).collect();
            (op.old_range(), text)
        })
        .collect()
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

/// Align equal lines on both sides, and in each block of deleted and inserted lines put the
/// lines [`matches`] finds alike on rows of their own, the lines between two of them side by side
/// in order and the longer side's rest beside `None`. Kept independent of the toolkit so
/// side-by-side alignment can be tested directly.
pub fn align(lines: &[DiffLine]) -> Vec<Row> {
    let mut rows = Vec::new();
    let (mut dels, mut ins) = (Vec::new(), Vec::new());
    for (i, line) in lines.iter().enumerate() {
        match line.op {
            Op::Equal => {
                flush(lines, &mut dels, &mut ins, &mut rows);
                rows.push(Row {
                    old: Some(i),
                    new: Some(i),
                });
            }
            // A delete after an insert starts a new pairing: `similar` emits deletes before
            // inserts within a hunk, so this only guards against input that does not.
            Op::Delete => {
                if !ins.is_empty() {
                    flush(lines, &mut dels, &mut ins, &mut rows);
                }
                dels.push(i);
            }
            Op::Insert => ins.push(i),
        }
    }
    flush(lines, &mut dels, &mut ins, &mut rows);
    rows
}

/// Lay one block of deleted and inserted lines (indices into `lines`) out as [`align`] says.
fn flush(lines: &[DiffLine], dels: &mut Vec<usize>, ins: &mut Vec<usize>, rows: &mut Vec<Row>) {
    let text = |i: &usize| lines[*i].text.as_str();
    let matched = matches(
        &dels.iter().map(text).collect::<Vec<_>>(),
        &ins.iter().map(text).collect::<Vec<_>>(),
    );
    let (mut a, mut b) = (0, 0);
    let end = (dels.len(), ins.len());
    for &(to_a, to_b) in matched.iter().chain(std::iter::once(&end)) {
        let (old, new) = (&dels[a..to_a], &ins[b..to_b]);
        for i in 0..old.len().max(new.len()) {
            rows.push(Row {
                old: old.get(i).copied(),
                new: new.get(i).copied(),
            });
        }
        if (to_a, to_b) != end {
            rows.push(Row {
                old: Some(dels[to_a]),
                new: Some(ins[to_b]),
            });
        }
        (a, b) = (to_a + 1, to_b + 1);
    }
    dels.clear();
    ins.clear();
}

/// The least [`likeness`] two changed lines need to be laid beside each other and refined word by
/// word.
const ALIKE: f32 = 0.5;

/// The most line pairs [`matches`] weighs in one block of deletions and insertions: 50 lines each
/// way, which it matches in about 1 ms (AMCOMP's P-cores, 2026-11-05). The work grows with the
/// pairs, and a diff laid where the reader waits pays it twice, in [`lines_within`] and in
/// [`align`]; a bigger block is a rewrite more than an edit, and matching it by position costs
/// nothing.
const MATCHED_PAIRS: usize = 2_500;

/// The lines of a block of deletions `old` and insertions `new` to lay beside each other, as index
/// pairs in order on both sides: pairs at least [`ALIKE`], together as alike as such pairs can be.
///
/// Matched by position, a line put in above edited ones stood beside the first of them, and every
/// edited line beside the one before its own, each refined against a line it had nothing to do
/// with: `similar` counts the spaces between words as shared, and a list's `-` and a `the` were
/// enough to lift two unrelated sentences over its floor. A block of more than [`MATCHED_PAIRS`]
/// is still matched by position.
fn matches(old: &[&str], new: &[&str]) -> Vec<(usize, usize)> {
    if old.len() * new.len() > MATCHED_PAIRS {
        return (0..old.len().min(new.len())).map(|i| (i, i)).collect();
    }
    let old_words: Vec<Vec<&str>> = old.iter().map(|line| words(line)).collect();
    let new_words: Vec<Vec<&str>> = new.iter().map(|line| words(line)).collect();
    // At `a * width + b`: the most likeness the pairs among `old[..a]` and `new[..b]` add up to,
    // and the step that got there, which the walk back from the end below retraces.
    let width = new.len() + 1;
    let mut best = vec![0.0f32; (old.len() + 1) * width];
    let mut took = vec![Step::Old; (old.len() + 1) * width];
    for a in 1..=old.len() {
        for b in 1..=new.len() {
            let at = a * width + b;
            (best[at], took[at]) = (best[at - width], Step::Old);
            if best[at - 1] > best[at] {
                (best[at], took[at]) = (best[at - 1], Step::New);
            }
            let texts = (old[a - 1], new[b - 1]);
            let alike = likeness(&old_words[a - 1], &new_words[b - 1], texts);
            if alike >= ALIKE && best[at - width - 1] + alike > best[at] {
                (best[at], took[at]) = (best[at - width - 1] + alike, Step::Both);
            }
        }
    }
    let (mut a, mut b, mut pairs) = (old.len(), new.len(), Vec::new());
    while a > 0 && b > 0 {
        match took[a * width + b] {
            Step::Old => a -= 1,
            Step::New => b -= 1,
            Step::Both => {
                (a, b) = (a - 1, b - 1);
                pairs.push((a, b));
            }
        }
    }
    pairs.reverse();
    pairs
}

/// How [`matches`] reached a pair of prefixes: leaving the last old line out, the last new one, or
/// matching the two.
#[derive(Clone, Copy)]
enum Step {
    Old,
    New,
    Both,
}

/// A line's words, the runs of letters, digits and `_`, sorted for [`likeness`]. The spaces and
/// the punctuation between them are left out, since unrelated lines share plenty of both.
fn words(line: &str) -> Vec<&str> {
    let mut words: Vec<&str> = (line.split(|c: char| !(c.is_alphanumeric() || c == '_')))
        .filter(|word| !word.is_empty())
        .collect();
    words.sort_unstable();
    words
}

/// How alike two lines are: twice the words they share over all their words, a word counting as
/// often as both have it. Two lines without a word are alike only when they read the same.
fn likeness(old: &[&str], new: &[&str], texts: (&str, &str)) -> f32 {
    if old.is_empty() && new.is_empty() {
        return match texts.0.trim() == texts.1.trim() {
            true => 1.0,
            false => 0.0,
        };
    }
    let (mut i, mut j, mut shared) = (0, 0, 0);
    while let (Some(x), Some(y)) = (old.get(i), new.get(j)) {
        match x.cmp(y) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => (i, j, shared) = (i + 1, j + 1, shared + 1),
        }
    }
    2.0 * shared as f32 / (old.len() + new.len()) as f32
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
    hunks_of(&changed(lines, rows))
}

/// Per row, whether it is anything but the same unchanged line on both sides.
fn changed(lines: &[DiffLine], rows: &[Row]) -> Vec<bool> {
    rows.iter().map(|row| !is_equal(lines, row)).collect()
}

/// The maximal runs of rows `changed` marks, as ranges: [`hunks`] over rows of any shape.
pub fn hunks_of(changed: &[bool]) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for (i, _) in changed.iter().enumerate().filter(|(_, c)| **c) {
        match out.last_mut() {
            Some(last) if last.end == i => last.end = i + 1,
            _ => out.push(i..i + 1),
        }
    }
    out
}

/// The fewest rows a gap hides: the button that stands for one or two is as tall as they are.
const MIN_GAP: usize = 3;

/// The rows a changes-only view hides, as ranges into `rows`: at least [`MIN_GAP`] long, sorted
/// and non-overlapping.
///
/// A run of unchanged rows between two hunks keeps `context` rows at each end, so it is hidden
/// only where it is longer than both margins together. The runs at the start and end of the file
/// have a hunk on one side only and keep one margin. A file with no hunks at all has nothing to
/// show, so all of it is one gap.
pub fn gaps(lines: &[DiffLine], rows: &[Row], context: usize) -> Vec<Range<usize>> {
    gaps_of(&changed(lines, rows), context)
}

/// [`gaps`] over rows of any shape, `changed` saying which rows are not unchanged everywhere.
pub fn gaps_of(changed: &[bool], context: usize) -> Vec<Range<usize>> {
    let hunks = hunks_of(changed);
    let (Some(first), Some(last)) = (hunks.first(), hunks.last()) else {
        // Nothing changed, so a changes-only view shows nothing and hides all of it.
        let whole = 0..changed.len();
        return if whole.len() < MIN_GAP {
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
    if changed.len() - last.end > context {
        out.push(last.end + context..changed.len());
    }
    out.retain(|gap| gap.len() >= MIN_GAP);
    out
}

/// One row of a three-column view: what [`align3`] puts beside a line of the middle text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row3 {
    /// Index into [`Three::left`] of the left text's line on this row.
    pub left: Option<usize>,
    /// The middle text's line, 1-based.
    pub mid: Option<usize>,
    /// Index into [`Three::right`] of the right text's line on this row.
    pub right: Option<usize>,
}

/// Two texts aligned beside a third: a merge's ours and theirs beside the file being resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct Three {
    /// The left text against the middle one, as [`lines`] gives it.
    pub left: Vec<DiffLine>,
    /// The right text against the middle one.
    pub right: Vec<DiffLine>,
    pub rows: Vec<Row3>,
}

impl Three {
    /// Per row, whether it is anything but one unchanged line in all three columns: what
    /// [`hunks_of`] and [`gaps_of`] take.
    pub fn changed(&self) -> Vec<bool> {
        let same =
            |lines: &[DiffLine], i: Option<usize>| i.is_some_and(|i| lines[i].op == Op::Equal);
        self.rows
            .iter()
            .map(|row| !(same(&self.left, row.left) && same(&self.right, row.right)))
            .collect()
    }
}

/// `left` and `right` each aligned against `mid`, as [`align`] aligns two texts, on one set of
/// rows: a line of `mid` is a row of its own with the line each side pairs it with, and the lines
/// a side has that `mid` lacks are rows before the `mid` line they come before, the two sides'
/// sharing rows. So a merge's ours stays level with the ours part of a conflict block in the file
/// and theirs with its theirs part, and each text keeps its own lines in order, each once.
pub fn align3(left: &str, mid: &str, right: &str) -> Three {
    let (l, r) = (lines(left, mid), lines(right, mid));
    let (ls, rs) = (beside(&l), beside(&r));
    let ends = ls.len();
    let mut rows = Vec::new();
    for (k, ((l_before, l_at), (r_before, r_at))) in ls.into_iter().zip(rs).enumerate() {
        for i in 0..l_before.len().max(r_before.len()) {
            rows.push(Row3 {
                left: l_before.get(i).copied(),
                mid: None,
                right: r_before.get(i).copied(),
            });
        }
        // The last entry is what comes after the middle text's last line.
        if k + 1 < ends {
            rows.push(Row3 {
                left: l_at,
                mid: Some(k + 1),
                right: r_at,
            });
        }
    }
    Three {
        left: l,
        right: r,
        rows,
    }
}

/// For each line of the new text, and once more for its end: the old text's lines before it
/// that the new text lacks, and the old line [`align`] puts beside it, as indices into `lines`.
fn beside(lines: &[DiffLine]) -> Vec<(Vec<usize>, Option<usize>)> {
    let (mut out, mut before) = (Vec::new(), Vec::new());
    for row in align(lines) {
        match row.new {
            Some(_) => out.push((std::mem::take(&mut before), row.old)),
            None => before.extend(row.old),
        }
    }
    out.push((before, None));
    out
}

/// Apply changes from `new` to `old` only on `side`'s `picked` lines (1-based, inclusive).
/// Stage Selected Lines uses the index as `old` and the working tree as `new`. Both use `\n`.
///
/// Selection follows aligned rows ([`align`]), including the opposite side of changed lines.
/// Deleted lines without a row on `side` belong to the preceding line.
pub fn apply_lines(old: &str, new: &str, side: Side, picked: RangeInclusive<usize>) -> String {
    mix(old, new, side, &picked, true)
}

/// `new` with the changes it makes on `side`'s lines `picked` undone, and no others: Unstage
/// Selected Lines, `old` being HEAD and `new` the index, and Revert Selected Lines, `old` being
/// the index and `new` the working tree. The selection is read as [`apply_lines`] reads it.
pub fn revert_lines(old: &str, new: &str, side: Side, picked: RangeInclusive<usize>) -> String {
    mix(old, new, side, &picked, false)
}

/// The lines of the other side facing `side`'s lines `span` (1-based, inclusive): those on the
/// rows the span covers, or, where none of those rows has one, the empty range between the lines
/// above and below them (`n + 1..=n`), which leaves those two equally near. What focus mode's line
/// fade is measured from in the column beside the editor.
pub fn facing(
    lines: &[DiffLine],
    rows: &[Row],
    side: Side,
    span: &RangeInclusive<usize>,
) -> RangeInclusive<usize> {
    let number = |side: Side, row: &Row| side.of(row).and_then(|i| side.number(&lines[i]));
    let first = rows
        .iter()
        .position(|row| number(side, row).is_some_and(|n| n >= *span.start()))
        .unwrap_or(rows.len());
    let end = rows
        .iter()
        .rposition(|row| number(side, row).is_some_and(|n| n <= *span.end()))
        .map_or(first, |last| (last + 1).max(first));
    let mut theirs = rows[first..end]
        .iter()
        .filter_map(|row| number(side.other(), row));
    match (theirs.next(), theirs.next_back()) {
        (Some(from), to) => from..=to.unwrap_or(from),
        (None, _) => {
            let above = rows[..first]
                .iter()
                .rev()
                .find_map(|row| number(side.other(), row))
                .unwrap_or(0);
            above + 1..=above
        }
    }
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
    fn a_hunk_is_refined_pair_by_pair() {
        let d = lines("a 1\nb 2\n", "a one\nb two\nc three\n");
        let emphasis: Vec<(&str, Vec<(usize, usize)>)> = (d.iter())
            .map(|l| {
                (
                    l.text.as_str(),
                    l.emphasis.iter().map(|r| (r.start, r.end)).collect(),
                )
            })
            .collect();
        assert_eq!(
            emphasis,
            vec![
                ("a 1", vec![(2, 3)]),
                ("b 2", vec![(2, 3)]),
                ("a one", vec![(2, 5)]),
                ("b two", vec![(2, 5)]),
                ("c three", vec![]),
            ],
            "each line against the one beside it; the line with no partner is left whole"
        );
    }

    /// Each row of `old` against `new` as [`align`] lays it, a side's line with its emphasis in
    /// brackets, and `None` where the side has blank space.
    fn laid(old: &str, new: &str) -> Vec<(Option<String>, Option<String>)> {
        let d = lines(old, new);
        let shown = |i: Option<usize>| {
            i.map(|i| {
                let (line, mut at, mut out) = (&d[i], 0, String::new());
                for r in &line.emphasis {
                    out += &format!("{}[{}]", &line.text[at..r.start], &line.text[r.clone()]);
                    at = r.end;
                }
                out + &line.text[at..]
            })
        };
        (align(&d).iter())
            .map(|r| (shown(r.old), shown(r.new)))
            .collect()
    }

    fn row(old: Option<&str>, new: Option<&str>) -> (Option<String>, Option<String>) {
        (old.map(str::to_string), new.map(str::to_string))
    }

    #[test]
    fn a_line_put_in_above_edited_ones_stands_alone_and_each_edit_faces_its_original() {
        let old = "- The quick brown fox jumps over the lazy dog.\n\
                   - A second item that stays nearly the same.\n";
        let new = "- An entirely new first item about last week.\n\
                   - The quick brown fox leaps over the lazy dog.\n\
                   - A second item that stays almost the same.\n";
        assert_eq!(
            laid(old, new),
            [
                row(None, Some("- An entirely new first item about last week.")),
                row(
                    Some("- The quick brown fox [jumps] over the lazy dog."),
                    Some("- The quick brown fox [leaps] over the lazy dog.")
                ),
                row(
                    Some("- A second item that stays [nearly] the same."),
                    Some("- A second item that stays [almost] the same.")
                ),
            ],
            "the new line has blank space beside it and no emphasis"
        );
        assert_eq!(
            apply_lines(old, new, Side::New, 2..=2),
            "- The quick brown fox leaps over the lazy dog.\n\
             - A second item that stays nearly the same.\n",
            "staging a line takes the one it is laid beside"
        );
    }

    #[test]
    fn of_two_lines_swapped_and_edited_the_closer_pair_faces_and_the_other_two_stand_alone() {
        let old = "Each run writes a file per sensor.\nThe readings are checked every day.\n";
        let new = "The readings are checked every week, then filed.\n\
                   Each run writes a file per channel.\n";
        assert_eq!(
            laid(old, new),
            [
                row(
                    None,
                    Some("The readings are checked every week, then filed.")
                ),
                row(
                    Some("Each run writes a file per [sensor.]"),
                    Some("Each run writes a file per [channel.]")
                ),
                row(Some("The readings are checked every day."), None),
            ]
        );
    }

    #[test]
    fn unlike_lines_between_two_alike_ones_stay_side_by_side_unrefined() {
        assert_eq!(
            laid("one two three\nfour\nfive\n", "one two six\nseven\n"),
            [
                row(Some("one two [three]"), Some("one two [six]")),
                row(Some("four"), Some("seven")),
                row(Some("five"), None),
            ]
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

    /// Twelve unchanged lines with the seventh rewritten: six rows lead up to the change and
    /// five follow it, so three rows of context leaves three rows to hide above and two below.
    /// The three are hidden; the two stay, a button standing for them saving no room.
    #[test]
    fn a_lone_change_hides_the_run_that_outgrows_its_context_by_three_rows() {
        let old_text: String = (1..=12).map(|i| format!("l{i}\n")).collect();
        let new_text = old_text.replace("l7\n", "L7\n");
        let d = lines(&old_text, &new_text);
        let rows = align(&d);
        assert_eq!(hunks(&d, &rows), vec![6..7]);
        assert_eq!(gaps(&d, &rows, 3), vec![0..3]);
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
    fn identical_texts_are_one_gap() {
        let same = "a\nb\nc\n";
        let d = lines(same, same);
        let rows = align(&d);
        assert!(hunks(&d, &rows).is_empty());
        assert_eq!(gaps(&d, &rows, 3), one(0..3));
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
    fn the_lines_facing_a_span_are_on_its_rows_or_either_side_of_the_blank() {
        // First and last, as a tuple: an empty range is what this answers beside a blank.
        let facing_in = |old, new, side, span| {
            let lines = lines(old, new);
            let facing = facing(&lines, &align(&lines), side, &span);
            (*facing.start(), *facing.end())
        };
        // `b` changed to `B`, `X` added beside `c` deleted, `a` and `d` the same.
        let (old, new) = ("a\nb\nc\nd\n", "a\nB\nX\nd\n");
        assert_eq!(facing_in(old, new, Side::New, 1..=1), (1, 1));
        assert_eq!(facing_in(old, new, Side::New, 3..=3), (3, 3), "on one row");
        assert_eq!(facing_in(old, new, Side::New, 2..=4), (2, 4));
        // Only added: the blank faces the lines either side of it.
        let (old, new) = ("a\nb\n", "a\nX\nY\nb\n");
        assert_eq!(facing_in(old, new, Side::New, 2..=3), (2, 1));
        assert_eq!(
            facing_in(old, new, Side::New, 5..=5),
            (3, 2),
            "past the end"
        );
        assert_eq!(
            facing_in(old, new, Side::Old, 2..=2),
            (4, 4),
            "the other way"
        );
    }

    /// [`align3`]'s rows as each column's line numbers, after checking that every column shows
    /// each of its text's lines once and in order.
    fn numbers3(left: &str, mid: &str, right: &str) -> Vec<[Option<usize>; 3]> {
        let three = align3(left, mid, right);
        let number = |lines: &[DiffLine], i: Option<usize>| i.and_then(|i| lines[i].old_line);
        let rows: Vec<[Option<usize>; 3]> = three
            .rows
            .iter()
            .map(|row| {
                [
                    number(&three.left, row.left),
                    row.mid,
                    number(&three.right, row.right),
                ]
            })
            .collect();
        for (c, text) in [left, mid, right].into_iter().enumerate() {
            let shown: Vec<usize> = rows.iter().filter_map(|row| row[c]).collect();
            let all: Vec<usize> = (1..=text.lines().count()).collect();
            assert_eq!(shown, all, "column {c}");
        }
        rows
    }

    #[test]
    fn three_columns_keep_each_side_beside_its_part_of_a_conflict_block() {
        let (ours, theirs) = ("a\nX\nc\n", "a\nY\nc\n");
        let file = "a\n<<<<<<< HEAD\nX\n=======\nY\n>>>>>>> side\nc\n";
        assert_eq!(
            numbers3(ours, file, theirs),
            [
                [Some(1), Some(1), Some(1)],
                [None, Some(2), None],
                [Some(2), Some(3), None],
                [None, Some(4), None],
                [None, Some(5), Some(2)],
                [None, Some(6), None],
                [Some(3), Some(7), Some(3)],
            ]
        );
        let three = align3(ours, file, theirs);
        assert_eq!(
            three.changed(),
            [false, true, true, true, true, true, false]
        );

        // Accept Current: theirs' line is now a change beside ours.
        let three = align3(ours, ours, theirs);
        assert_eq!(three.changed(), [false, true, false]);
        assert_eq!(
            numbers3(ours, ours, theirs),
            [
                [Some(1), Some(1), Some(1)],
                [Some(2), Some(2), Some(2)],
                [Some(3), Some(3), Some(3)]
            ]
        );
        assert_eq!(three.right[three.rows[1].right.unwrap()].op, Op::Delete);
        assert!(!align3(ours, ours, ours).changed().contains(&true));
    }

    #[test]
    fn lines_both_sides_add_in_one_place_share_rows() {
        assert_eq!(
            numbers3("a\nL1\nL2\nb\n", "a\nb\n", "a\nR1\nb\nz\n"),
            [
                [Some(1), Some(1), Some(1)],
                [Some(2), None, Some(2)],
                [Some(3), None, None],
                [Some(4), Some(2), Some(3)],
                [None, None, Some(4)],
            ]
        );
    }

    #[test]
    fn three_columns_hide_unchanged_runs_as_two_do() {
        let base: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        let changed = base.replace("line 10\n", "line ten\n");
        let three = align3(&changed, &base, &base);
        let two = lines(&changed, &base);
        assert_eq!(
            gaps_of(&three.changed(), 3),
            gaps(&two, &align(&two), 3),
            "one changed row either way"
        );
        assert_eq!(gaps_of(&three.changed(), 3), [0..6, 13..20]);
        assert_eq!(hunks_of(&three.changed()), vec![9..10]);
    }

    #[test]
    fn a_text_read_again_replaces_only_the_lines_that_differ() {
        let old = "a\nb\nc\nd\ne";
        let new = "a\nB\nc\nd\nx\ne\nf";
        let stretches = replaced(old, new);
        // Put back together from the end, as a buffer is edited, it reads `new`.
        let mut lines: Vec<String> = old.split_inclusive('\n').map(String::from).collect();
        for (range, text) in stretches.iter().rev() {
            lines.splice(range.clone(), [text.clone()]);
        }
        assert_eq!(lines.concat(), new);
        // The lines both have are in none of the stretches.
        for kept in [0, 2, 3] {
            assert!(stretches.iter().all(|(range, _)| !range.contains(&kept)));
        }
        assert_eq!(stretches[0], (1..2, "B\n".to_string()));
        assert!(replaced(old, old).is_empty());
    }

    #[test]
    fn reverting_lines_undoes_the_selected_changes_and_keeps_the_rest() {
        assert_eq!(
            revert_lines("a\nb\nc\n", "a\nB\nc\nd\n", Side::New, 2..=2),
            "a\nb\nc\nd\n"
        );
    }
}
