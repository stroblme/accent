//! Accepting a row: where its edits land, the text having moved on since it was asked for.

use accent_api::{Completion, Pos, Range, TextEdit};
use gtk::prelude::*;

use crate::lang;

/// Where an answer was asked for, and how many characters have been typed there since.
///
/// An answer's positions are about the text it was asked about. Typing at the caret moves what
/// is after it and nothing before it, so a position before the caret stands and one after it moves
/// on by what was typed. Where the two meet, the end of a range decides: one starting at the
/// caret starts before what was typed and one ending there ends after it, so the word typed since
/// the popup opened is part of what a row replaces (`get` + `get_func` once gave `getget_func`),
/// and so is the `]]` a note's link eats after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Moved {
    pub at: Pos,
    pub typed: u32,
}

impl Moved {
    /// Where a range starting at `pos` starts now.
    pub(super) fn start(&self, pos: Pos) -> Pos {
        self.shift(pos, pos.character > self.at.character)
    }

    /// Where a range ending at `pos` ends now.
    pub(super) fn end(&self, pos: Pos) -> Pos {
        self.shift(pos, pos.character >= self.at.character)
    }

    fn shift(&self, pos: Pos, after: bool) -> Pos {
        match pos.line == self.at.line && after {
            true => Pos {
                line: pos.line,
                character: pos.character + self.typed,
            },
            false => pos,
        }
    }
}

/// What accepting an item replaces now: its range moved on by what was typed, and reaching the
/// caret at least, should a server's range end short of it.
pub(super) fn replaced(range: Range, moved: Moved, caret: Pos) -> Range {
    let (start, end) = (moved.start(range.start), moved.end(range.end));
    match caret.line == end.line && caret > end {
        true => Range { start, end: caret },
        false => Range { start, end },
    }
}

/// `edits` in the order they can be applied without invalidating each other: last in the document
/// first, so an edit never moves the range of one still to come.
fn ordered(mut edits: Vec<TextEdit>) -> Vec<TextEdit> {
    edits.sort_by_key(|e| std::cmp::Reverse((e.range.start, e.text.len())));
    edits
}

/// Write `item` over `range`, as one user action so one Ctrl+Z takes the whole of it back: the
/// replaced text, the insert, and whatever import the item brought with it. A snippet goes to
/// `push_snippet`, which parks it in the view for its stops to be walked.
pub(super) fn apply(
    buffer: &gtk::TextBuffer,
    item: &Completion,
    range: Range,
    push_snippet: impl FnOnce(&sourceview5::Snippet, &mut gtk::TextIter),
) {
    buffer.begin_user_action();
    let mut start = lang::iter_at(buffer, range.start);
    let mut end = lang::iter_at(buffer, range.end);
    buffer.delete(&mut start, &mut end);
    // `delete` leaves both iters at the deletion point, so this writes exactly there.
    match item.is_snippet {
        // A snippet the parser refuses goes in as the text it is, which is wrong in a small way
        // rather than losing the acceptance altogether.
        true => match sourceview5::Snippet::new_parsed(&item.insert) {
            Ok(snippet) => push_snippet(&snippet, &mut start),
            Err(e) => {
                tracing::debug!("cannot parse the snippet {:?}: {e}", item.insert);
                buffer.insert(&mut start, &item.insert);
            }
        },
        false => buffer.insert(&mut start, &item.insert),
    }
    // They sit before the caret in practice — an import at the top of the file — which is why
    // they can be applied after the insert at all.
    for edit in ordered(item.extra_edits.clone()) {
        let mut from = lang::iter_at(buffer, edit.range.start);
        let mut to = lang::iter_at(buffer, edit.range.end);
        buffer.delete(&mut from, &mut to);
        buffer.insert(&mut from, &edit.text);
    }
    buffer.end_user_action();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(character: u32) -> Pos {
        Pos { line: 3, character }
    }

    fn range(start: u32, end: u32) -> Range {
        Range {
            start: at(start),
            end: at(end),
        }
    }

    #[test]
    fn what_was_typed_since_the_ask_is_replaced_with_the_rest() {
        // Asked at `s.|`, then `ge` typed: the word grows into the range.
        let moved = Moved {
            at: at(5),
            typed: 2,
        };
        assert_eq!(replaced(range(5, 5), moved, at(7)), range(5, 7));
        // A note's `[[De|]]`: the `]]` it eats moves on with the typing.
        let moved = Moved {
            at: at(4),
            typed: 3,
        };
        assert_eq!(replaced(range(0, 6), moved, at(7)), range(0, 9));
        // A range ending short of the caret still takes what was typed up to it.
        let still = Moved {
            at: at(5),
            typed: 0,
        };
        assert_eq!(replaced(range(2, 4), still, at(5)), range(2, 5));
        // Another line moves with nothing typed on this one.
        let other = Range {
            start: Pos {
                line: 0,
                character: 0,
            },
            end: Pos {
                line: 0,
                character: 0,
            },
        };
        assert_eq!(
            Moved {
                at: at(5),
                typed: 2
            }
            .start(other.start),
            other.start
        );
    }

    fn edit(line: u32, text: &str) -> TextEdit {
        let at = Pos { line, character: 0 };
        TextEdit {
            range: Range { start: at, end: at },
            text: text.to_string(),
        }
    }

    /// Applying an edit moves everything after it, so the extra edits an item brings are applied
    /// from the end of the document backwards and none of them is ever asked about a stale
    /// position.
    #[test]
    fn extra_edits_are_applied_last_first() {
        let order: Vec<String> = ordered(vec![edit(0, "a"), edit(12, "c"), edit(4, "b")])
            .into_iter()
            .map(|e| e.text)
            .collect();
        assert_eq!(order, ["c", "b", "a"]);
    }
}
