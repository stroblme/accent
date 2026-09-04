//! Line diff of two texts.
//!
//! Deliberately about two strings and nothing else: the conflict view of Phase 1 and the git
//! views of Phase 4 are the same widget over the same rows.

use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Op {
    Equal,
    Delete,
    Insert,
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
                op: match c.tag() {
                    ChangeTag::Equal => Op::Equal,
                    ChangeTag::Delete => Op::Delete,
                    ChangeTag::Insert => Op::Insert,
                },
                old_line: c.old_index().map(|i| i + 1),
                new_line: c.new_index().map(|i| i + 1),
                text,
                emphasis,
            }
        })
        .collect()
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
}
