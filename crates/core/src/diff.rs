//! Line diff of two texts.
//!
//! Deliberately about two strings and nothing else: the conflict view of Phase 1 and the git
//! views of Phase 4 are the same widget over the same rows.

use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};

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
}

/// Line diff of two texts, oldest-first, suitable for a side-by-side view.
pub fn lines(old: &str, new: &str) -> Vec<DiffLine> {
    let diff = TextDiff::from_lines(old, new);
    diff.iter_all_changes()
        .map(|c| DiffLine {
            op: match c.tag() {
                ChangeTag::Equal => Op::Equal,
                ChangeTag::Delete => Op::Delete,
                ChangeTag::Insert => Op::Insert,
            },
            old_line: c.old_index().map(|i| i + 1),
            new_line: c.new_index().map(|i| i + 1),
            text: c.value().trim_end_matches(['\r', '\n']).to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(op: Op, old: Option<usize>, new: Option<usize>, text: &str) -> DiffLine {
        DiffLine {
            op,
            old_line: old,
            new_line: new,
            text: text.to_string(),
        }
    }

    #[test]
    fn line_diff_marks_equal_delete_and_insert() {
        let d = lines("alpha\nbravo\ncharlie\n", "alpha\nbravo two\ncharlie\n");
        assert_eq!(
            d,
            vec![
                line(Op::Equal, Some(1), Some(1), "alpha"),
                line(Op::Delete, Some(2), None, "bravo"),
                line(Op::Insert, None, Some(2), "bravo two"),
                line(Op::Equal, Some(3), Some(3), "charlie"),
            ]
        );
    }
}
