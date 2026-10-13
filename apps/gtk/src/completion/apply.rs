//! Map completion edits to current text after typing beyond the request.

use std::collections::HashSet;

use accent_api::language::byte_of;
use accent_api::{Completion, Pos, Range, TextEdit};
use gtk::prelude::*;

use crate::lang;

/// Track the request position and characters typed there since the answer was requested.
///
/// Positions before the caret stay fixed; later ones shift. At the caret, range starts
/// precede typed text and ends include it, so accepting replaces the growing prefix rather
/// than duplicating it and still consumes trailing wikilink brackets.
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

    /// Where `edit` goes now, an extra edit of the answer's.
    pub(super) fn edit(&self, edit: &TextEdit) -> TextEdit {
        TextEdit {
            range: Range {
                start: self.start(edit.range.start),
                end: self.end(edit.range.end),
            },
            text: edit.text.clone(),
        }
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

/// Map the accepted range through subsequent typing, extending to at
/// least the caret even when the server supplied a shorter end.
pub(super) fn replaced(range: Range, moved: Moved, caret: Pos) -> Range {
    let (start, end) = (moved.start(range.start), moved.end(range.end));
    match caret.line == end.line && caret > end {
        true => Range { start, end: caret },
        false => Range { start, end },
    }
}

/// Order edits back to front so each preserves positions of later operations.
fn ordered(mut edits: Vec<TextEdit>) -> Vec<TextEdit> {
    edits.sort_by_key(|e| std::cmp::Reverse((e.range.start, e.text.len())));
    edits
}

/// Append snippet `$0` when absent, preventing GtkSourceView 5.20's
/// `_gtk_source_snippet_insert_set` assertion on Tab past the last stop
/// (as with `editor::lines::chunks`).
fn with_final_stop(snippet: &str) -> String {
    let has = |stop: &str| {
        snippet
            .match_indices(stop)
            .any(|(at, _)| !snippet[..at].ends_with('\\'))
    };
    match has("$0") || has("${0:") || has("${0}") {
        true => snippet.to_string(),
        false => format!("{snippet}$0"),
    }
}

/// Turn each number's first bare `$N` into `${N}` unless it has a `${N…}` already: LSP's `$N` is
/// a stop, GtkSourceView 5.20's a mirror of stop N's text, so a `foo($1)` walked nothing. Repeats
/// stay mirrors, as LSP has them; escapes, variables and what is inside `${…}` are left alone.
fn with_stops(snippet: &str) -> String {
    let b = snippet.as_bytes();
    let digits = |at: usize| at + b[at..].iter().take_while(|c| c.is_ascii_digit()).count();
    // The top level's `$N` and `${N`, as their digits' span and whether braced.
    let mut found = Vec::new();
    let (mut i, mut depth) = (0, 0);
    while i < b.len() {
        match (b[i], b.get(i + 1)) {
            (b'\\', _) => i += 1,
            (b'$', Some(b'{')) => {
                if depth == 0 {
                    found.push((i + 2, digits(i + 2), true));
                }
                depth += 1;
                i += 1;
            }
            (b'$', _) if depth == 0 => found.push((i + 1, digits(i + 1), false)),
            (b'}', _) if depth > 0 => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    let stop = |(at, end): (usize, usize)| snippet[at..end].parse::<u32>().ok().filter(|&n| n > 0);
    let mut seen: HashSet<u32> = found
        .iter()
        .filter(|f| f.2)
        .filter_map(|&(at, end, _)| stop((at, end)))
        .collect();
    let mut out = String::with_capacity(snippet.len() + 2);
    let mut from = 0;
    for (at, end, braced) in found {
        if !braced && stop((at, end)).is_some_and(|n| seen.insert(n)) {
            out.push_str(&snippet[from..at]);
            out.push('{');
            out.push_str(&snippet[at..end]);
            out.push('}');
            from = end;
        }
    }
    out.push_str(&snippet[from..]);
    out
}

/// Apply `item`, `range` and `extras` in current coordinates as one undoable action.
///
/// Apply extra edits first, then the item over marks adjusted through those edits. Snippets
/// must come last because later edits would be interpreted inside their Tab stops. Extras
/// at range boundaries remain outside the item replacement.
pub(super) fn apply(
    buffer: &gtk::TextBuffer,
    item: &Completion,
    range: Range,
    extras: Vec<TextEdit>,
    push_snippet: impl FnOnce(&sourceview5::Snippet, &mut gtk::TextIter),
) {
    buffer.begin_user_action();
    let start = buffer.create_mark(None, &lang::iter_at(buffer, range.start), false);
    let end = buffer.create_mark(None, &lang::iter_at(buffer, range.end), true);
    replace(buffer, extras);
    let mut from = buffer.iter_at_mark(&start);
    let mut to = buffer.iter_at_mark(&end);
    buffer.delete_mark(&start);
    buffer.delete_mark(&end);
    if to < from {
        to = from;
    }
    buffer.delete(&mut from, &mut to);
    // `delete` leaves both iters at the deletion point, so this writes exactly there. A snippet
    // the parser refuses goes in as the text it is, which is wrong in a small way rather than
    // losing the acceptance altogether.
    let snippet = item
        .is_snippet
        .then(|| sourceview5::Snippet::new_parsed(&with_final_stop(&with_stops(&item.insert))));
    match snippet {
        Some(Ok(snippet)) => push_snippet(&snippet, &mut from),
        Some(Err(e)) => {
            tracing::debug!("cannot parse the snippet {:?}: {e}", item.insert);
            buffer.insert(&mut from, &item.insert);
        }
        None => buffer.insert(&mut from, &item.insert),
    }
    buffer.end_user_action();
}

/// Apply what a resolve that landed after the acceptance added, as one undoable action.
pub(super) fn edits(buffer: &gtk::TextBuffer, edits: Vec<TextEdit>) {
    buffer.begin_user_action();
    replace(buffer, edits);
    buffer.end_user_action();
}

fn replace(buffer: &gtk::TextBuffer, edits: Vec<TextEdit>) {
    for edit in ordered(edits) {
        let mut from = lang::iter_at(buffer, edit.range.start);
        let mut to = lang::iter_at(buffer, edit.range.end);
        buffer.delete(&mut from, &mut to);
        buffer.insert(&mut from, &edit.text);
    }
}

/// Whether `edits`, a late resolve's, still go where they say in `now`, the text as it was when
/// the row went in being `then`: nothing up to the last of them has changed since, so none of
/// their positions moved. Text changed only after them moves none, which is VS Code's rule.
pub(super) fn still_fits(then: &str, now: &str, edits: &[TextEdit]) -> bool {
    let Some(end) = edits.iter().map(|e| e.range.end).max() else {
        return true;
    };
    byte_of(then, end).is_some_and(|at| now.get(..at) == Some(&then[..at]))
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

    /// Verify extra edits apply back to front at current positions,
    /// including imports above and below the caret.
    #[test]
    fn extra_edits_are_applied_last_first() {
        let order: Vec<String> = ordered(vec![edit(0, "a"), edit(12, "c"), edit(4, "b")])
            .into_iter()
            .map(|e| e.text)
            .collect();
        assert_eq!(order, ["c", "b", "a"]);
    }

    #[test]
    fn an_extra_edit_after_the_caret_moves_with_the_typing() {
        let moved = Moved {
            at: at(5),
            typed: 2,
        };
        let after = TextEdit {
            range: range(9, 10),
            text: ")".into(),
        };
        assert_eq!(moved.edit(&after).range, range(11, 12));
        let before = TextEdit {
            range: range(0, 1),
            text: "x".into(),
        };
        assert_eq!(moved.edit(&before).range, range(0, 1));
    }

    /// A late import goes in while the typing since is all after it, and not once the text before
    /// it has changed.
    #[test]
    fn a_late_edit_fits_while_the_text_before_it_is_unchanged() {
        let then = "use a;\nfn main() {\n    s.gamma();\n}\n";
        let import = [edit(1, "use g;\n")];
        let below = then.replace("gamma();", "gamma(); x");
        assert!(still_fits(then, &below, &import));
        assert!(!still_fits(then, &format!("// x\n{then}"), &import));
        assert!(!still_fits(
            then,
            &then.replacen("use a;\n", "", 1),
            &import
        ));
        assert!(
            !still_fits(then, "use", &import),
            "the text it lands after is cut short"
        );
        assert!(still_fits(then, "", &[]));
    }

    #[test]
    fn a_snippet_gets_a_final_stop_unless_it_has_one() {
        assert_eq!(with_final_stop("add($1, $2)"), "add($1, $2)$0");
        assert_eq!(with_final_stop("add($1)$0"), "add($1)$0");
        assert_eq!(with_final_stop("${0:x} + $1"), "${0:x} + $1");
        assert_eq!(with_final_stop("cost: \\$0"), "cost: \\$0$0");
    }

    #[test]
    fn a_bare_stop_becomes_one_gtksourceview_walks() {
        assert_eq!(with_stops("foo($1)"), "foo(${1})");
        assert_eq!(with_stops("foo($1, $2)$0"), "foo(${1}, ${2})$0");
        assert_eq!(with_stops("$1 $1"), "${1} $1", "a repeat mirrors");
        assert_eq!(with_stops("$1 ${1:a}"), "$1 ${1:a}", "the stop is braced");
        assert_eq!(with_stops("\\$1 \\\\$1"), "\\$1 \\\\${1}");
        assert_eq!(with_stops("${1:a$2} $2"), "${1:a$2} ${2}");
        assert_eq!(
            with_stops("$TM_FILENAME ${TM_LINE:x$1}"),
            "$TM_FILENAME ${TM_LINE:x$1}"
        );
    }
}
