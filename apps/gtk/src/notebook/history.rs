//! A notebook's one undo history: the typing in its cells and the operations on its cells, in the
//! order they happened (user decision 2026-10-19). The cells' buffers keep no undo of their own;
//! what is typed is recorded here, a run of characters typed or rubbed out in one place being one
//! step, as GtkSourceView runs them together, broken at the start of a word.

use std::time::{Duration, Instant};

use accent_core::notebook::{CellId, Edit};

/// One change to a cell's text: at character `at`, `removed` gave way to `inserted`.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub at: i32,
    pub removed: String,
    pub inserted: String,
}

#[derive(Debug)]
pub enum Step {
    /// One edit of a cell's text, all its changes made at once.
    Text { cell: CellId, changes: Vec<Change> },
    /// A cell operation, as the model edits that walk it back or forward again.
    Cells(Vec<Edit>),
}

/// How long a pause ends a run of typing.
const PAUSE: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// When the step on top was last typed into, while the next keystroke may run on into it.
    typed: Option<Instant>,
}

impl History {
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// A cell operation was made, `undo` being the edits that take it back.
    pub fn push_cells(&mut self, undo: Vec<Edit>) {
        self.redo.clear();
        self.typed = None;
        self.undo.push(Step::Cells(undo));
    }

    /// `cell`'s text changed by `changes`, at `now`: a keystroke that runs on from the one before
    /// joins its step.
    pub fn push_text(&mut self, cell: CellId, changes: Vec<Change>, now: Instant) {
        self.redo.clear();
        let recent = self.typed.is_some_and(|at| now.duration_since(at) < PAUSE);
        if let (
            true,
            Some(Step::Text {
                cell: on,
                changes: before,
            }),
            [change],
        ) = (recent, self.undo.last_mut(), &changes[..])
            && *on == cell
            && let Some(last) = before.last_mut()
            && runs_on(last, change)
        {
            self.typed = Some(now);
            return;
        }
        self.typed = typed(&changes).then_some(now);
        self.undo.push(Step::Text { cell, changes });
    }

    /// The step to undo, which the caller walks back and hands to [`undone`](Self::undone).
    pub fn take_undo(&mut self) -> Option<Step> {
        self.typed = None;
        self.undo.pop()
    }

    pub fn take_redo(&mut self) -> Option<Step> {
        self.typed = None;
        self.redo.pop()
    }

    /// `step` walks forward again what was just undone.
    pub fn undone(&mut self, step: Step) {
        self.redo.push(step);
    }

    /// `step` walks back again what was just redone.
    pub fn redone(&mut self, step: Step) {
        self.undo.push(step);
    }

    /// The next keystroke starts a step of its own: the caret went somewhere else.
    pub fn seal(&mut self) {
        self.typed = None;
    }
}

/// Whether `changes` is one character typed or rubbed out, which the next may run on from.
fn typed(changes: &[Change]) -> bool {
    match changes {
        [c] => {
            let one = |s: &str| s.chars().count() == 1 && s != "\n";
            (c.removed.is_empty() && one(&c.inserted)) || (c.inserted.is_empty() && one(&c.removed))
        }
        _ => false,
    }
}

/// Fold `next`, one character typed or rubbed out, into `last` if it carries on from it: typing
/// after typing, Backspace before Backspace, Delete at Delete. A space after a word starts a new
/// step, so a sentence is undone a word at a time.
fn runs_on(last: &mut Change, next: &Change) -> bool {
    if !typed(std::slice::from_ref(next)) {
        return false;
    }
    let len = |s: &str| s.chars().count() as i32;
    let word_ends = |before: &str, after: &str| {
        after.starts_with(char::is_whitespace)
            && !before.ends_with(char::is_whitespace)
            && !before.is_empty()
    };
    match (
        next.removed.is_empty(),
        last.removed.is_empty(),
        last.inserted.is_empty(),
    ) {
        // Typing on.
        (true, true, false) if next.at == last.at + len(&last.inserted) => {
            if word_ends(&last.inserted, &next.inserted) {
                return false;
            }
            last.inserted.push_str(&next.inserted);
            true
        }
        // Backspace, the run growing leftwards.
        (false, false, true) if next.at + len(&next.removed) == last.at => {
            last.at = next.at;
            last.removed.insert_str(0, &next.removed);
            true
        }
        // Delete, the run growing rightwards from the same place.
        (false, false, true) if next.at == last.at => {
            last.removed.push_str(&next.removed);
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typing(at: i32, text: &str) -> Vec<Change> {
        vec![Change {
            at,
            removed: String::new(),
            inserted: text.into(),
        }]
    }

    fn steps(h: &History) -> Vec<String> {
        h.undo
            .iter()
            .map(|s| match s {
                Step::Text { changes, .. } => changes
                    .iter()
                    .map(|c| format!("{}-{:?}+{:?}", c.at, c.removed, c.inserted))
                    .collect(),
                Step::Cells(_) => "cells".into(),
            })
            .collect()
    }

    #[test]
    fn typing_runs_together_a_word_at_a_time() {
        let (mut h, now) = (History::default(), Instant::now());
        let cell = accent_core::notebook::read("a.py", "import marimo\napp = marimo.App()\n")
            .unwrap()
            .new_cell(accent_core::notebook::Kind::Code, "")
            .unwrap()
            .id();
        for (i, c) in "ab cd".chars().enumerate() {
            h.push_text(cell, typing(i as i32, &c.to_string()), now);
        }
        assert_eq!(steps(&h), ["0-\"\"+\"ab\"", "2-\"\"+\" cd\""]);
        // Two Backspaces run together; a pause, a paste or a cell operation starts a new step.
        let rub = |at, c: &str| {
            vec![Change {
                at,
                removed: c.into(),
                inserted: String::new(),
            }]
        };
        h.push_text(cell, rub(4, "d"), now);
        h.push_text(cell, rub(3, "c"), now);
        assert_eq!(steps(&h)[2], "3-\"cd\"+\"\"");
        h.push_text(cell, typing(3, "x"), now + PAUSE);
        h.push_cells(Vec::new());
        h.push_text(cell, typing(4, "y"), now + PAUSE);
        h.push_text(cell, typing(5, "pasted"), now + PAUSE);
        assert_eq!(steps(&h).len(), 7);
        assert!(h.can_undo() && !h.can_redo());
    }
}
