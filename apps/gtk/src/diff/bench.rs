//! Comparison state and actions exposed to drills.

use accent_core::diff;
use adw::prelude::*;

use super::columns::Columns;
use super::links::Ends;
use super::{Compare, Side, first_change};

impl Compare {
    /// Right-minus-left column start in window pixels; expected zero.
    /// [`Compare::misaligned`] uses buffer coordinates and cannot detect this.
    pub fn skew(&self) -> i32 {
        let top = |side: Side| {
            self.pane(side)
                .scroller
                .compute_point(&self.columns.paned, &gtk::graphene::Point::zero())
                .map_or(0.0, |p| p.y())
        };
        (top(Side::New) - top(Side::Old)).round() as i32
    }

    /// Whether rows and saved viewport position have settled before a drill acts.
    pub fn settled(&self) -> bool {
        self.asked.get().is_none()
            && self.columns.keep.get().is_none()
            && self.columns.pending.borrow().is_none()
    }

    /// Whether the first hunk is visible on its first nonempty side,
    /// the required opening position.
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

    /// GTK y positions of first-hunk and following rows, old/new per row.
    /// `None` means no visible line on that side.
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

    /// The divider between the columns.
    pub fn paned(&self) -> &gtk::Paned {
        &self.columns.paned
    }

    /// Show All Unchanged Lines.
    pub fn unfold(&self) -> &gtk::ToggleButton {
        &self.unfold
    }

    /// What the relayout passes did since the last call: see `Columns::passes`.
    pub fn passes(&self) -> String {
        self.columns.passes()
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

    /// Count rows differing from intended alignment height; expected zero.
    pub fn misaligned(&self) -> usize {
        self.columns.misaligned()
    }

    /// Count visible rows whose peer lines have unequal GTK heights and
    /// describe the first as `row:old_y/new_y` relative to viewport top.
    /// `None` while an edit leaves the row mapping stale.
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
        let start = |r: usize, side: Side| {
            let n = side.number(&lines[side.of(&rows[r])?])?;
            Some(starts[side.idx()][n - 1])
        };
        let y = |r: usize, side: Side| {
            let pane = self.pane(side);
            let at = pane.buffer.iter_at_offset(start(r, side)?);
            Some(pane.view.iter_location(&at).y() - pane.view.visible_rect().y())
        };
        // The characters on screen in each column: a row with no line starting there is not
        // asked about, asking GTK where every row is laying the whole text out.
        let shown = [Side::Old, Side::New].map(|side| {
            let view = &self.pane(side).view;
            let seen = view.visible_rect();
            let mut end = view.line_at_y(seen.y() + seen.height()).0;
            end.forward_line();
            view.line_at_y(seen.y()).0.offset()..end.offset()
        });
        let on_screen = |r: usize| {
            [Side::Old, Side::New]
                .into_iter()
                .any(|side| start(r, side).is_some_and(|at| shown[side.idx()].contains(&at)))
        };
        let (mut count, mut first) = (0, String::new());
        for r in 0..rows.len() {
            if !on_screen(r) || self.hides_row(r) {
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

    /// Describe the first [`Compare::misaligned`] row: side, expected and
    /// actual GTK positions, and line text.
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

    /// Row `r`'s line on `side` as laid out: see [`Columns::laid`].
    fn laid(&self, r: usize, side: Side) -> Option<(i32, i32, i32, i32)> {
        self.columns.laid(r, side.idx())
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

    /// The shown strip buttons named `label`: see [`Columns::strip_buttons`].
    pub fn strip_buttons(&self, label: &str) -> Vec<(i32, gtk::Button)> {
        self.columns.strip_buttons(label)
    }

    /// The strip as laid now: see [`Columns::strip`].
    pub fn strip(&self) -> String {
        self.columns.strip()
    }

    /// What the button on the `i`th hidden run does.
    pub fn open_gap(&self, i: usize) {
        let key = self.hidden.borrow().get(i).map(|(_, key)| *key);
        if let Some(key) = key {
            self.open_run(key);
        }
    }
}

impl Columns {
    /// Count rows differing from intended height in any column; expected zero.
    pub(super) fn misaligned(&self) -> usize {
        let rows = self.rows.borrow().changed.len();
        if self.grid.borrow().tops.len() != rows {
            return rows;
        }
        let off = |r, c| {
            self.laid(r, c)
                .is_some_and(|(expected, actual, ..)| expected != actual)
        };
        (0..rows)
            .filter(|&r| (0..self.panes.len()).any(|c| off(r, c)))
            .count()
    }

    /// Return row `r`/column `c` intended and GTK y positions, line height
    /// and row height in buffer pixels. `None` when the column has no line.
    pub(super) fn laid(&self, r: usize, c: usize) -> Option<(i32, i32, i32, i32)> {
        let (rows, grid) = (self.rows.borrow(), self.grid.borrow());
        let own = grid.heights[c].get(r).copied().flatten()?;
        let n = rows.lines[c][r]?;
        let tallest = grid
            .heights
            .iter()
            .map(|h| h[r].unwrap_or(0))
            .max()
            .unwrap_or(0)
            + grid.extra[r];
        let pane = &self.panes[c];
        let expected = grid.tops[r] + pane.view.pixels_above_lines();
        let iter = pane.buffer.iter_at_offset(rows.starts[c][n - 1]);
        Some((expected, pane.view.iter_location(&iter).y(), own, tallest))
    }

    /// Whether the rows are laid and the view is where it was being kept: what a drill waits for.
    pub(super) fn settled(&self) -> bool {
        self.keep.get().is_none() && self.pending.borrow().is_none()
    }

    /// The shown strip buttons named `label`, top to bottom, each with the `y` its band starts at
    /// in buffer coordinates.
    pub(super) fn strip_buttons(&self, label: &str) -> Vec<(i32, gtk::Button)> {
        let grid = self.grid.borrow();
        let mut buttons: Vec<(i32, gtk::Button)> = (self.links.buttons().into_iter())
            .filter(|(_, name, _)| *name == label)
            .filter_map(|(row, _, button)| Some((*grid.tops.get(row)?, button)))
            .collect();
        buttons.sort_by_key(|(y, _)| *y);
        buttons
    }

    /// Return strip geometry and alignment counts: `over=0` for buttons
    /// over columns, `stale=0` for misplaced buttons and `links_off=0` for
    /// connector endpoints missing their rows.
    pub(super) fn strip(&self) -> String {
        let (columns, strips) = self.links.widths();
        let (buttons, rects, over, stale) = self.links.rects(self);
        let (_, off) = self.links_check();
        format!(
            "widths={columns:?}/{strips:?} buttons={buttons:?} columns={rects:?} over={over} \
             stale={stale} links_off={off}"
        )
    }

    /// Connector `(top, bottom)` endpoints per strip side in drawing pixels,
    /// plus count not matching GTK row positions; expected zero, as with
    /// [`Columns::misaligned`] for rows.
    pub(super) fn links_check(&self) -> (Vec<Vec<Ends>>, usize) {
        let ends: Vec<Vec<Ends>> = (0..self.panes.len() - 1)
            .map(|i| self.links.ends(i, self))
            .collect();
        let rows = self.rows.borrow();
        // Where GTK draws row `r`, in the drawing's pixels: in the first column with a line there.
        let drawn = |r: usize| -> Option<i32> {
            if rows.hidden.iter().any(|(gap, _)| gap.contains(&r)) {
                return None;
            }
            (0..self.panes.len()).find_map(|c| {
                let n = rows.lines[c].get(r).copied().flatten()?;
                let view = &self.panes[c].view;
                let at = self.panes[c].buffer.iter_at_offset(rows.starts[c][n - 1]);
                let y = view.iter_location(&at).y() - view.pixels_above_lines();
                let (_, y) = view.buffer_to_window_coords(gtk::TextWindowType::Widget, 0, y);
                let point = gtk::graphene::Point::new(0.0, y as f32);
                let at = view.compute_point(self.links.area(), &point)?;
                Some(at.y().round() as i32)
            })
        };
        let mut off = 0;
        for (i, ends) in ends.iter().enumerate() {
            let runs = self.links.runs(i);
            off += runs.len().abs_diff(ends.len());
            for (run, end) in runs.iter().zip(ends) {
                for (span, (top, bottom)) in run.iter().zip(end) {
                    for (r, y) in [(span.start, *top), (span.end, *bottom)] {
                        off += usize::from(drawn(r).is_some_and(|at| at != y));
                    }
                }
            }
        }
        (ends, off)
    }
}
