//! What the drills read off a comparison, and the presses they make on it.

use accent_core::diff;
use adw::prelude::*;

use super::{Compare, Side, first_change};

impl Compare {
    /// How many pixels lower the right column starts than the left one in the window: 0 is the
    /// claim, and what [`Compare::misaligned`] cannot see, being in buffer coordinates.
    pub fn skew(&self) -> i32 {
        let top = |side: Side| {
            self.pane(side)
                .scroller
                .compute_point(&self.columns.paned, &gtk::graphene::Point::zero())
                .map_or(0.0, |p| p.y())
        };
        (top(Side::New) - top(Side::Old)).round() as i32
    }

    /// Whether the rows are laid and the view is where the comparison was keeping it: what a drill
    /// waits for before it acts on a comparison just opened.
    pub fn settled(&self) -> bool {
        self.columns.keep.get().is_none() && self.columns.pending.borrow().is_none()
    }

    /// Whether the first hunk's first line is inside its view right now, on the first side that
    /// has a line in it: what a comparison has to open on.
    pub fn first_hunk_on_screen(&self) -> bool {
        let Some((side, at)) = self.first_hunk_line() else {
            return false;
        };
        let view = &self.pane(side).view;
        let (line, seen) = (
            view.iter_location(&self.pane(side).buffer.iter_at_offset(at)),
            view.visible_rect(),
        );
        line.y() >= seen.y() && line.y() + line.height() <= seen.y() + seen.height()
    }

    /// Where the first hunk starts, in characters, on the first side with a line in it.
    fn first_hunk_line(&self) -> Option<(Side, i32)> {
        let (lines, rows, starts) = (
            self.lines.borrow(),
            self.rows.borrow(),
            self.starts.borrow(),
        );
        let hunk = diff::hunks(&lines, &rows).into_iter().next()?;
        [Side::Old, Side::New].into_iter().find_map(|side| {
            let n = hunk
                .clone()
                .find_map(|r| side.of(&rows[r]).and_then(|i| side.number(&lines[i])))?;
            Some((side, starts[side.idx()][n - 1]))
        })
    }

    /// Where the first hunk's lines and the ones after it start, as GTK lays them out: the row,
    /// then the old and the new side's `y`, `None` where a side has no visible line in the row.
    pub fn first_hunk_tops(&self) -> Vec<(usize, Option<i32>, Option<i32>)> {
        let hunk = {
            let (lines, rows) = (self.lines.borrow(), self.rows.borrow());
            diff::hunks(&lines, &rows).first().cloned()
        };
        let Some(hunk) = hunk else {
            return Vec::new();
        };
        let end = (hunk.end + 1).min(self.rows.borrow().len());
        let top = |r, side| self.laid(r, side).map(|(_, actual, ..)| actual);
        (hunk.start..end)
            .map(|r| (r, top(r, Side::Old), top(r, Side::New)))
            .collect()
    }

    /// The shared vertical scrollbar, for the bench to read and to move as a reader would.
    pub fn vadjustment(&self) -> gtk::Adjustment {
        self.columns.panes[0].scroller.vadjustment()
    }

    /// Whether row `r` is in a hidden run right now.
    pub fn hides_row(&self, r: usize) -> bool {
        self.hidden.borrow().iter().any(|(gap, _)| gap.contains(&r))
    }

    /// Where the first change starts on the editor's side, in characters.
    pub fn opens_at(&self) -> Option<i32> {
        let side = self.editable?;
        let (lines, rows) = (self.lines.borrow(), self.rows.borrow());
        first_change(&lines, &rows, &self.starts.borrow()[side.idx()], side)
    }

    /// How many rows GTK lays out at a different height than the last relayout meant them to
    /// have, on either side: the number the alignment stands or falls on, and 0 is the claim.
    pub fn misaligned(&self) -> usize {
        let rows = self.rows.borrow().len();
        if self.columns.grid.borrow().tops.len() != rows {
            return rows;
        }
        let off = |r, side| {
            self.laid(r, side)
                .is_some_and(|(expected, actual, ..)| expected != actual)
        };
        (0..rows)
            .filter(|&r| off(r, Side::Old) || off(r, Side::New))
            .count()
    }

    /// The rows on screen whose two lines GTK draws at different heights right now, and the first
    /// of them spelled out (`row:old_y/new_y`, below the top of the view). `None` while an edit has
    /// not been laid over yet, when the rows say nothing about the text.
    pub fn uneven(&self) -> Option<(usize, String)> {
        let (lines, rows, starts) = (
            self.lines.borrow(),
            self.rows.borrow(),
            self.starts.borrow(),
        );
        let fresh = |side: Side| {
            starts[side.idx()].last().copied() == Some(self.pane(side).buffer.char_count())
        };
        if !fresh(Side::Old) || !fresh(Side::New) {
            return None;
        }
        let height = self.columns.panes[0].view.visible_rect().height();
        let y = |r: usize, side: Side| {
            let n = side.number(&lines[side.of(&rows[r])?])?;
            let pane = self.pane(side);
            let at = pane.buffer.iter_at_offset(starts[side.idx()][n - 1]);
            Some(pane.view.iter_location(&at).y() - pane.view.visible_rect().y())
        };
        let (mut count, mut first) = (0, String::new());
        for r in 0..rows.len() {
            if self.hides_row(r) {
                continue;
            }
            let (Some(old), Some(new)) = (y(r, Side::Old), y(r, Side::New)) else {
                continue;
            };
            if old != new && [old, new].iter().any(|y| (0..height).contains(y)) {
                if count == 0 {
                    first = format!("{r}:{old}/{new}");
                }
                count += 1;
            }
        }
        Some((count, first))
    }

    /// The first row [`Compare::misaligned`] counts, spelled out: which row and side, what the
    /// relayout expected, what GTK laid out, and the line. For the bench to print.
    pub fn first_misaligned(&self) -> Option<String> {
        let rows = self.rows.borrow().len();
        (0..rows)
            .flat_map(|r| [Side::Old, Side::New].map(|side| (r, side)))
            .find_map(|(r, side)| {
                let (expected, actual, own, tallest) =
                    self.laid(r, side).filter(|(e, a, ..)| e != a)?;
                let (lines, rows) = (self.lines.borrow(), self.rows.borrow());
                let text = side
                    .of(&rows[r])
                    .map(|i| lines[i].text.clone())
                    .unwrap_or_default();
                Some(format!(
                    "row={r} side={side:?} expected={expected} actual={actual} own={own} tallest={tallest} text={text:?}"
                ))
            })
    }

    /// Row `r`'s line on `side` as laid out: where the last relayout meant it to start, where GTK
    /// put it, its own height and its row's, in buffer pixels. `None` where the side has no line.
    fn laid(&self, r: usize, side: Side) -> Option<(i32, i32, i32, i32)> {
        let (lines, rows, starts) = (
            self.lines.borrow(),
            self.rows.borrow(),
            self.starts.borrow(),
        );
        let grid = self.columns.grid.borrow();
        let own = grid.heights[side.idx()].get(r).copied().flatten()?;
        let n = side.of(&rows[r]).and_then(|i| side.number(&lines[i]))?;
        let tallest = grid.heights[0][r]
            .unwrap_or(0)
            .max(grid.heights[1][r].unwrap_or(0))
            + grid.extra[r];
        let pane = self.pane(side);
        let expected = grid.tops[r] + pane.view.pixels_above_lines();
        let iter = pane.buffer.iter_at_offset(starts[side.idx()][n - 1]);
        Some((expected, pane.view.iter_location(&iter).y(), own, tallest))
    }

    /// What the `Take` (or, with `keep_own`, the `Both`) button on the `i`th hunk does.
    pub fn take_hunk(&self, i: usize, keep_own: bool) {
        let hunk = {
            let (lines, rows) = (self.lines.borrow(), self.rows.borrow());
            diff::hunks(&lines, &rows).get(i).cloned()
        };
        if let Some(hunk) = hunk {
            self.take(hunk, keep_own);
        }
    }

    /// What the button on the `i`th hidden run does.
    pub fn open_gap(&self, i: usize) {
        let key = self.hidden.borrow().get(i).map(|(_, key)| *key);
        if let Some(key) = key {
            self.open_run(key);
        }
    }
}
