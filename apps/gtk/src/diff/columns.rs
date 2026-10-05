//! What a comparison of any number of columns stands on: one vertical scroll they share, the
//! padding that keeps row `i` level in every column, the buttons laid over the rows, and the line
//! the reader is on kept where it is while the rows are laid again.
//!
//! Knows nothing about diffs. The host — [`super::Compare`] — works out what each row shows in
//! each column and hands it over as [`Rows`]; everything here is measured from the views.

use adw::prelude::*;
use gtk::glib;
use sourceview5::prelude::*;
use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::{Rc, Weak};

use super::Pane;
use super::pad::{UNMEASURED, bands, carried, is_pad, measure, pad, padding, reclaim, unmeasured};
use crate::editor;

/// Where the first hunk lands when a comparison opens, as a fraction of the view's height. A
/// quarter down rather than at the top, so the lines that lead up to the change are visible too.
const FIRST_HUNK_AT: f64 = 0.25;

/// Blank space a hidden run leaves behind, for the button that opens it to sit in.
pub(super) const GAP_PX: i32 = 28;
/// Inset of the hunk buttons from the pane's right edge.
const INSET: i32 = 8;
/// How often a relayout asks again for the heights GTK had not validated yet, and how long it
/// waits between asking: GTK validates a screenful per idle, so a few are enough for any note.
const SETTLE: u8 = 10;
const SETTLE_AFTER: std::time::Duration = std::time::Duration::from_millis(100);

/// Where the overlaid buttons sit, in rows.
#[derive(Clone, Copy)]
pub(super) enum Anchor {
    /// At the right end of the row's top: the Take / Keep Both pair of the hunk starting there.
    Hunk(usize),
    /// Centred in the blank space a hidden run left at this row.
    Gap(usize),
    /// In the blank space the host left at this row on purpose ([`Rows::extra`]), at its start,
    /// centre or end.
    Room(usize, gtk::Align),
}

/// Where the view is kept until the rows are laid: see [`Columns::keep`].
#[derive(Clone, Copy)]
pub(super) enum Keep {
    /// The first hunk at [`FIRST_HUNK_AT`] of the page, which is where a comparison opens.
    FirstHunk,
    /// The scroll a hidden run was opened at, held there: see [`Columns::hold_scroll`].
    Scroll(f64),
    /// Line `.1` of column `.0`, its row `.2` pixels below the top of the view: see
    /// [`Columns::top_line`] and [`Columns::hold_line`].
    Line(usize, usize, i32),
}

/// What the host lays the columns out from, laid again on every refresh.
#[derive(Default)]
pub(super) struct Rows {
    /// Per column and row, the 1-based line the column shows there, `None` where it has none.
    pub(super) lines: Vec<Vec<Option<usize>>>,
    /// Per column, where each line of its text starts, in characters (`line_starts`).
    pub(super) starts: Vec<Vec<i32>>,
    /// Per row, whether it is a change: the blank that levels it goes under a changed line.
    pub(super) changed: Vec<bool>,
    /// The rows hidden right now.
    pub(super) hidden: Vec<Range<usize>>,
    /// Per row, the blank space every column leaves there on purpose, for buttons to sit in.
    pub(super) extra: Vec<i32>,
    /// Per column, the hue of the blank it leaves where it has no line in a change.
    pub(super) hues: Vec<(f32, f32, f32)>,
    /// The first hunk's first row, where a comparison opens.
    pub(super) first: Option<usize>,
    /// The buttons laid over the rows, by column.
    pub(super) overlays: Vec<(usize, gtk::Widget, Anchor)>,
}

/// What a relayout measured: every row's natural height per column (`None` where the column has
/// no visible line), the space every column leaves at a row on purpose, and where each row starts
/// in the shared grid.
#[derive(Default)]
pub(super) struct Grid {
    #[cfg(feature = "bench")]
    pub(super) heights: Vec<Vec<Option<i32>>>,
    #[cfg(feature = "bench")]
    pub(super) extra: Vec<i32>,
    pub(super) tops: Vec<i32>,
}

pub(super) struct Columns {
    weak: Weak<Columns>,
    pub(super) panes: Vec<Pane>,
    /// Which column is the user's own editor, if any. Its text is read, never set.
    pub(super) editable: Option<usize>,
    pub(super) paned: gtk::Paned,
    /// What the host laid last.
    pub(super) rows: RefCell<Rows>,
    /// The grid the last relayout laid down, kept for the bench's checks.
    pub(super) grid: RefCell<Grid>,
    pub(super) pending: RefCell<Option<glib::SourceId>>,
    /// How many more times a relayout that had to estimate a height may ask GTK again. Reset
    /// by every refresh; a bound, because a line GTK never validates would otherwise be asked
    /// about forever.
    settling: Cell<u8>,
    /// Each column's own vertical adjustment, given up for the first column's while the
    /// comparison lasts and handed back by [`Columns::leave`].
    own: Vec<gtk::Adjustment>,
    /// The bottom margin each view was last given here, and how much of it is the blank under a
    /// column with no line: see [`Columns::page_bottom`].
    bottoms: Vec<Cell<(i32, i32)>>,
    /// Where the view is kept until a relayout has laid every row, which clears it. The first
    /// hunk, as the comparison is built: a diff opens on what changed rather than on the top of a
    /// file whose first difference is four hundred lines down. The scroll a run was opened at (see
    /// [`Columns::hold_scroll`]). After that where the view sits is the reader's business.
    pub(super) keep: Cell<Option<Keep>>,
    handlers: RefCell<Vec<(glib::Object, glib::SignalHandlerId)>>,
}

impl Drop for Columns {
    /// The read-only columns go with the comparison: see [`editor::release`]. The editor's view
    /// is its tab's.
    fn drop(&mut self) {
        for (i, pane) in self.panes.iter().enumerate() {
            if self.editable != Some(i) {
                editor::release(&pane.view);
            }
        }
    }
}

/// Put `scroller` on `adjustment`. GTK 4.22's `set_vadjustment` leaves the overlay scrollbar's
/// fade handler, which a realized scroller connects to its adjustment, on the one it leaves; once
/// the scroller is freed, the next scroll of that adjustment runs the handler on freed memory.
/// Overlay scrolling switched off and back on around the swap takes the handler along.
fn swap_vadjustment(scroller: &gtk::ScrolledWindow, adjustment: &gtk::Adjustment) {
    let overlay = scroller.is_overlay_scrolling();
    scroller.set_overlay_scrolling(false);
    scroller.set_vadjustment(Some(adjustment));
    scroller.set_overlay_scrolling(overlay);
}

/// The columns' roots side by side, a divider between each two: a paned of the first and the
/// rest.
fn split(roots: &[&gtk::Widget]) -> gtk::Paned {
    let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    let rest = match roots {
        [_, last] => (*last).clone(),
        rest => split(&rest[1..]).upcast(),
    };
    paned.set_start_child(Some(roots[0]));
    paned.set_end_child(Some(&rest));
    paned.set_resize_start_child(true);
    paned.set_shrink_start_child(false);
    paned.set_resize_end_child(true);
    paned.set_shrink_end_child(false);
    paned
}

impl Columns {
    /// `panes` side by side, the first on the left; `editable` names the one whose buffer is the
    /// user's.
    pub(super) fn new(panes: Vec<Pane>, editable: Option<usize>) -> Rc<Self> {
        // Vertical is shared, so views of the same rows cannot drift apart. Horizontal stays per
        // pane: everything wraps, so there is nothing to scroll sideways anyway.
        let own: Vec<gtk::Adjustment> = panes.iter().map(|p| p.scroller.vadjustment()).collect();
        for pane in &panes[1..] {
            swap_vadjustment(&pane.scroller, &own[0]);
        }
        // One height for every title row: the editor's carries Stop Comparing and would stand
        // taller, starting its column, and every row in it, that much lower. The group lives as
        // long as the rows do.
        let titles = gtk::SizeGroup::new(gtk::SizeGroupMode::Vertical);
        // And one minimum width for every column, which is what splits them evenly: a paned whose
        // position was never set divides its width in the ratio of the two, on every allocation
        // until a drag sets one. A position set from an idle once the paned was mapped was lost
        // whenever the idle ran before the first allocation, and the columns stayed split by
        // their own minimums, the editor's the wider for its Stop Comparing.
        let widths = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        for pane in &panes {
            titles.add_widget(&pane.header);
            widths.add_widget(&pane.root);
        }
        let roots: Vec<&gtk::Widget> = panes.iter().map(|p| &p.root).collect();
        let paned = split(&roots);
        let n = panes.len();

        let this = Rc::new_cyclic(|weak| Columns {
            weak: weak.clone(),
            panes,
            editable,
            paned,
            rows: RefCell::new(Rows::default()),
            grid: RefCell::new(Grid::default()),
            pending: RefCell::new(None),
            settling: Cell::new(0),
            own,
            bottoms: (0..n).map(|_| Cell::default()).collect(),
            keep: Cell::new(Some(Keep::FirstHunk)),
            handlers: RefCell::new(Vec::new()),
        });

        // Weak throughout: every handler below is connected to something the comparison owns,
        // and a strong capture is a cycle.
        let weak = this.weak.clone();
        let connect = |object: glib::Object, id: glib::SignalHandlerId| {
            this.handlers.borrow_mut().push((object, id));
        };
        for pane in &this.panes {
            // Everything wraps, so a pane's width is its horizontal page size: a paned drag or a
            // window resize lands here and re-measures the rows, the line at the top staying.
            let w = weak.clone();
            let hadj = pane.scroller.hadjustment();
            // GTK notifies on every allocation, the same width or not.
            let width = Cell::new(hadj.page_size());
            let id = hadj.connect_page_size_notify(move |hadj| {
                let Some(c) = w.upgrade() else { return };
                if width.replace(hadj.page_size()) != hadj.page_size() {
                    c.rewrapped();
                }
                c.schedule_relayout();
            });
            connect(hadj.upcast(), id);
        }
        // GTK lays lines out lazily and the total height moves as it reaches them, as it does
        // on a font change; the rows are re-measured each time it settles.
        let w = weak.clone();
        let vadj = this.own[0].clone();
        let id = vadj.connect_upper_notify(move |_| {
            if let Some(c) = w.upgrade() {
                c.schedule_relayout();
            }
        });
        connect(vadj.clone().upcast(), id);
        // A scroll held by `Columns::hold_scroll`, or a line kept by `Compare::set_side`, goes
        // back to where it is held.
        let w = weak.clone();
        let id = vadj.connect_value_changed(move |adj| {
            let Some(c) = w.upgrade() else { return };
            match c.keep.get() {
                Some(Keep::Scroll(value)) if adj.value() != value => adj.set_value(value),
                Some(Keep::Line(column, n, at)) => c.hold_line(column, n, at),
                _ => {}
            }
        });
        connect(vadj.upcast(), id);
        // Text typed ahead of a line's padding goes back under it on the keystroke itself: above
        // 16 KB the editor refreshes the comparison only on its debounce, and the line would be
        // laid out bare until then.
        if let Some(mine) = editable {
            let buffer = this.panes[mine].buffer.clone();
            let id = buffer.connect_changed(reclaim);
            connect(buffer.clone().upcast(), id);
            // And the lines an edit touches are marked as GTK's to lay out again, before the
            // buffer's `changed` lays the comparison over them: see `pad::measure`.
            let id = buffer.connect_insert_text(|buffer, at, _| unmeasured(buffer, at, at));
            connect(buffer.clone().upcast(), id);
            let id = buffer.connect_delete_range(unmeasured);
            connect(buffer.upcast(), id);
        }
        this
    }

    /// Lay the columns out from `rows`: the host's every refresh.
    pub(super) fn lay(&self, rows: Rows) {
        *self.rows.borrow_mut() = rows;
        self.settling.set(SETTLE);
        self.relayout();
    }

    /// Whether the view is still waiting to go to the first hunk, which nothing else may move.
    pub(super) fn opening(&self) -> bool {
        matches!(self.keep.get(), Some(Keep::FirstHunk))
    }

    /// Hold the scroll where it is until the rows are laid: see `Compare::open_run`.
    pub(super) fn hold_scroll(&self) {
        let value = self.own[0].value();
        self.keep.set(Some(Keep::Scroll(value)));
    }

    /// The editor's caret line, and how far below the top of the view its row starts, while that
    /// row starts on screen.
    pub(super) fn caret_line(&self) -> Option<Keep> {
        let mine = self.editable?;
        let buffer = &self.panes[mine].buffer;
        let n = buffer.iter_at_mark(&buffer.get_insert()).line() as usize + 1;
        let (rows, grid) = (self.rows.borrow(), self.grid.borrow());
        let row = rows
            .lines
            .get(mine)?
            .iter()
            .position(|&line| line == Some(n))?;
        let seen = self.panes[0].view.visible_rect();
        let at = grid.tops.get(row)? - seen.y();
        (0..seen.height())
            .contains(&at)
            .then_some(Keep::Line(mine, n, at))
    }

    /// A new width rewraps every line, and each view keeps its own top line in place as GTK lays
    /// them out again, all on the one scroll they share: the line at the top is held instead,
    /// until the rows have settled at the new width.
    fn rewrapped(&self) {
        if self.keep.get().is_none() {
            self.hold(self.top_line(self.editable.unwrap_or(self.panes.len() - 1)));
            self.settling.set(SETTLE);
        }
    }

    /// Keep `keep` until the rows are laid, a [`Keep::Line`] held again after each layout GTK
    /// makes meanwhile, before it is painted: the lines GTK lays out in the frame itself move a
    /// kept line without moving the scroll.
    pub(super) fn hold(&self, keep: Option<Keep>) {
        let held = matches!(self.keep.replace(keep), Some(Keep::Line(..)));
        let clock = self.panes[0].view.frame_clock();
        let (false, Some(Keep::Line(..)), Some(clock)) = (held, keep, clock) else {
            return;
        };
        let id: Rc<Cell<Option<glib::SignalHandlerId>>> = Rc::default();
        let (w, own) = (self.weak.clone(), id.clone());
        id.set(Some(clock.connect_layout(
            move |clock| match w.upgrade().map(|c| (c.keep.get(), c)) {
                Some((Some(Keep::Line(column, n, at)), c)) => c.hold_line(column, n, at),
                _ => {
                    if let Some(id) = own.take() {
                        clock.disconnect(id);
                    }
                }
            },
        )));
    }

    /// Where the reader is, as a line of `column`: the one in the row at the top of the view, or in
    /// the nearest row above it that has one, and how far below the top of the view its row starts.
    pub(super) fn top_line(&self, column: usize) -> Option<Keep> {
        let (rows, grid) = (self.rows.borrow(), self.grid.borrow());
        let lines = rows.lines.get(column)?;
        let seen = self.panes[0].view.visible_rect().y();
        let top = grid.tops.partition_point(|&y| y <= seen).max(1);
        let (row, n) = lines[..top.min(lines.len())]
            .iter()
            .enumerate()
            .rev()
            .find_map(|(r, line)| Some((r, (*line)?)))?;
        Some(Keep::Line(column, n, grid.tops[row] - seen))
    }

    /// Scroll so line `n` of `column` starts `at` pixels below the top of the view where GTK has
    /// it now, which is where it is drawn: what keeps a [`Keep::Line`] on screen while the rows are
    /// laid. GTK lays out the lines that opened, hid or grew above it a few at a time, and keeps
    /// each view's own top line in place as it does, all on the one scroll they share; a scroll
    /// held where it was showed the lines above for as long as that took.
    fn hold_line(&self, column: usize, n: usize, at: i32) {
        let pane = &self.panes[column];
        let Some(line) = pane.buffer.iter_at_line(n as i32 - 1) else {
            return;
        };
        let y = pane.view.line_yrange(&line).0;
        let adj = &self.own[0];
        let value = adj.value() + f64::from(y - pane.view.visible_rect().y() - at);
        if value != adj.value() {
            adj.set_value(value);
        }
    }

    /// Put the first hunk at [`FIRST_HUNK_AT`] of the page, once.
    fn reveal_first_hunk(&self) {
        // Nothing has changed yet — an untouched buffer against its own index side. The next
        // relayout that finds a difference is the one that opens on it.
        let first = self.rows.borrow().first;
        if let Some(row) = first {
            let page = self.own[0].page_size();
            self.reveal(row, FIRST_HUNK_AT * page);
        }
    }

    /// Put row `row` `at` pixels below the top of the view, from the rows the last relayout
    /// measured, and let the view go. Every pane shares the vertical adjustment, so setting it
    /// scrolls them all.
    ///
    /// The grid rather than GTK's own figures, and not before the relayout: GTK lays lines out
    /// lazily and the padding just laid is not in its figures yet, so a `scroll_to_mark` made as
    /// the comparison opened landed wherever the estimates put the line, which in a long file
    /// with its unchanged runs folded away was nowhere near it.
    fn reveal(&self, row: usize, at: f64) {
        let Some(top) = self.grid.borrow().tops.get(row).copied() else {
            return;
        };
        self.keep.set(None);
        // `visible_rect` is in buffer coordinates and the adjustment is not — the top margin
        // lies between them — so the scroll moves by the distance from what is on screen now.
        let (adj, seen) = (&self.own[0], self.panes[0].view.visible_rect());
        adj.set_value(adj.value() + f64::from(top - seen.y()) - at);
    }

    /// Everything of the layout off every pane: what leaving a comparison does before the panes
    /// part, once the host has taken its own tags off.
    pub(super) fn leave(&self) {
        for pane in &self.panes {
            let (start, end) = pane.buffer.bounds();
            let mut pads = Vec::new();
            pane.buffer.tag_table().foreach(|tag| {
                if is_pad(tag) || tag.name().as_deref() == Some(UNMEASURED) {
                    pads.push(tag.clone());
                }
            });
            for tag in pads {
                pane.buffer.remove_tag(&tag, &start, &end);
            }
            pane.pool.unclaim();
            pane.pool.hide_unclaimed();
            if let Some(view) = pane.view.downcast_ref::<crate::multicaret::View>() {
                view.set_bands(Vec::new());
            }
        }
        self.rows.borrow_mut().overlays.clear();
        if let Some(id) = self.pending.borrow_mut().take() {
            id.remove();
        }
        for (object, id) in self.handlers.borrow_mut().drain(..) {
            object.disconnect(id);
        }
        // Each column scrolls on its own again before the companion can go. A GtkTextView that is
        // freed stays connected to its adjustment, so one left on the adjustment the editor keeps
        // was called into after it was gone: a comparison left the moment it opened — a Changes
        // row git had outgrown — crashed the window on the editor's next scroll.
        for (pane, own) in self.panes.iter().zip(&self.own).skip(1) {
            swap_vadjustment(&pane.scroller, own);
        }
        // And the editor its page's bottom margin, should it have been the column with no line.
        if let Some(mine) = self.editable {
            self.set_bottom(mine, self.page_bottom(mine), 0);
        }
    }

    /// Put the editor's page on the companions beside it: the margins, so the first row of each
    /// starts level, the line spacing, and the tab width the Indent Width preference sets. The
    /// heading markers hang in the left margin and are measured in the font, so they are hung
    /// again where the margin moves and, with `refont`, after a font change, which only the
    /// editor's tab hears of. The bottom margin is [`Columns::relayout`]'s.
    pub(super) fn follow_editor(&self, refont: bool) {
        let Some(mine) = self.editable else {
            return;
        };
        let from = &self.panes[mine].view;
        for (i, companion) in self.panes.iter().enumerate() {
            if i == mine {
                continue;
            }
            let to = &companion.view;
            let rehang = refont || to.left_margin() != from.left_margin();
            to.set_top_margin(from.top_margin());
            to.set_left_margin(from.left_margin());
            to.set_right_margin(from.right_margin());
            to.set_pixels_above_lines(from.pixels_above_lines());
            to.set_pixels_below_lines(from.pixels_below_lines());
            to.set_tab_width(from.tab_width());
            if rehang {
                editor::rehang_companion(companion.flavour, &companion.buffer, to);
            }
        }
    }

    /// The bottom margin the page gives `column`, without the blank a relayout left under a column
    /// with no line: the editor's own, which a companion beside it takes as well. A margin
    /// someone else has set since — the zoom — is the page's whole.
    fn page_bottom(&self, column: usize) -> i32 {
        let column = self.editable.unwrap_or(column);
        let (set, blank) = self.bottoms[column].get();
        let now = self.panes[column].view.bottom_margin();
        if now == set { now - blank } else { now }
    }

    /// `column`'s bottom margin: the page's, and `blank` pixels more under a column with no line.
    fn set_bottom(&self, column: usize, page: i32, blank: i32) {
        self.panes[column].view.set_bottom_margin(page + blank);
        self.bottoms[column].set((page + blank, blank));
    }

    pub(super) fn schedule_relayout(&self) {
        if self.pending.borrow().is_some() {
            return;
        }
        let weak = self.weak.clone();
        let id = glib::idle_add_local_once(move || {
            if let Some(c) = weak.upgrade() {
                *c.pending.borrow_mut() = None;
                c.relayout();
            }
        });
        *self.pending.borrow_mut() = Some(id);
    }

    /// Measure every row in every column and pad the shorter ones, then put the buttons where the
    /// rows now are. Idempotent: a row whose padding has not changed is left alone, so a pass
    /// that finds nothing to do invalidates nothing and the layout settles.
    ///
    /// ponytail: every visible row is laid out on every pass, which is a Pango layout per
    /// paragraph — fine at the few hundred rows a note has, and debounced by the editor above
    /// that. Measuring only the rows an edit touched is the upgrade if a long note shows it.
    fn relayout(&self) {
        if !self.panes.iter().all(|p| p.view.is_mapped()) {
            return;
        }
        // Before anything is measured: a keystroke's refresh gets here ahead of the buffer's own
        // `changed` handler.
        if let Some(mine) = self.editable {
            reclaim(&self.panes[mine].buffer);
        }
        self.follow_editor(false);
        let rows = self.rows.borrow();
        // Nothing laid yet.
        if rows.lines.len() != self.panes.len() {
            return;
        }
        let count = rows.changed.len();
        let is_hidden = |r: usize| rows.hidden.iter().any(|gap| gap.contains(&r));
        let estimated = Cell::new(false);
        let heights: Vec<Vec<Option<i32>>> = (self.panes.iter().enumerate())
            .map(|(c, pane)| {
                let st = &rows.starts[c];
                (0..count)
                    .map(|r| {
                        if is_hidden(r) {
                            return None;
                        }
                        let n = rows.lines[c][r]?;
                        let (above, below) =
                            carried(&pane.view, &pane.buffer.iter_at_offset(st[n - 1]));
                        let (height, estimate) =
                            measure(&pane.view, &pane.buffer, st[n - 1], st[n], above + below);
                        estimated.set(estimated.get() || estimate);
                        Some(height)
                    })
                    .collect()
            })
            .collect();
        let mut extra = rows.extra.clone();
        for gap in &rows.hidden {
            extra[gap.start] += GAP_PX;
        }
        let (pads, tops) = padding(&heights, &extra, &rows.changed);
        // A column with no line at all has no paragraph to pad, so the blank that keeps it as tall
        // as the others goes under its text, less the one empty line it shows. Left shorter, its
        // view pulled the scroll they share back into its own range whenever it was laid out,
        // and a file a commit added could not be scrolled at all.
        let pages: Vec<i32> = (0..self.panes.len()).map(|c| self.page_bottom(c)).collect();
        for (c, pane) in self.panes.iter().enumerate() {
            let blank = match pads[c].rest {
                0 => 0,
                rest => rest - pane.view.line_yrange(&pane.buffer.start_iter()).1,
            };
            self.set_bottom(c, pages[c], blank.max(0));
        }
        for (c, pane) in self.panes.iter().enumerate() {
            if let Some(view) = pane.view.downcast_ref::<crate::multicaret::View>() {
                let hue = rows.hues[c];
                view.set_bands(bands(&heights, &extra, &rows.changed, &tops, c, hue));
            }
        }

        let mut repadded = false;
        for (c, pane) in self.panes.iter().enumerate() {
            let st = &rows.starts[c];
            let now = &pads[c];
            // Every line in this column, hidden ones included, so a row that is hidden now or was
            // the last one before an edit does not keep what it carried then.
            for (r, line) in rows.lines[c].iter().enumerate() {
                let Some(n) = *line else {
                    continue;
                };
                repadded |= pad(
                    &pane.view,
                    &pane.buffer,
                    st[n - 1],
                    st[n],
                    now.above[r],
                    now.below[r],
                );
            }
        }

        // Buffer coordinates, which start at the first paragraph — the view's top margin is
        // outside them — and scroll with the text.
        for (c, widget, anchor) in rows.overlays.iter() {
            let view = &self.panes[*c].view;
            let width = view.visible_rect().width();
            let (_, wanted, _, _) = widget.measure(gtk::Orientation::Horizontal, -1);
            let (_, height, _, _) = widget.measure(gtk::Orientation::Vertical, -1);
            let (x, y) = match *anchor {
                Anchor::Hunk(row) => (width - wanted - INSET, tops[row]),
                Anchor::Gap(row) => ((width - wanted) / 2, tops[row] + (GAP_PX - height) / 2),
                Anchor::Room(row, align) => {
                    let x = match align {
                        gtk::Align::Start => INSET,
                        gtk::Align::End => width - wanted - INSET,
                        _ => (width - wanted) / 2,
                    };
                    (x, tops[row] + (rows.extra[row] - height) / 2)
                }
            };
            view.move_overlay(widget, x.max(0), y);
        }
        *self.grid.borrow_mut() = Grid {
            #[cfg(feature = "bench")]
            heights,
            #[cfg(feature = "bench")]
            extra,
            tops,
        };
        // Every view laid out again in the next frame, ahead of painting any. GTK lays a view out
        // from an idle of its own and repaints it once it has, so one column could reach the
        // screen a frame ahead of another: a keystroke's own line in the editor a frame after
        // the padding that answers it beside it.
        if repadded {
            for pane in &self.panes {
                pane.view.queue_allocate();
            }
        }
        // GTK had not laid some line out yet: ask again once it has. So too while the view waits
        // to be put somewhere for a pass that moved nothing: padding GTK has not laid out yet is
        // not in where the scroll puts a line, and each view keeps its top line where it was as it
        // catches up.
        let unsettled = estimated.get() || (self.keep.get().is_some() && repadded);
        let exhausted = self.settling.get() == 0;
        if unsettled && !exhausted && self.pending.borrow().is_none() {
            self.settling.set(self.settling.get() - 1);
            let weak = self.weak.clone();
            let id = glib::timeout_add_local_once(SETTLE_AFTER, move || {
                if let Some(c) = weak.upgrade() {
                    *c.pending.borrow_mut() = None;
                    c.relayout();
                }
            });
            *self.pending.borrow_mut() = Some(id);
        }
        // Once every row is measured and laid as the grid has it, or GTK has been asked as often
        // as it will be. After the borrows: moving the scroll runs handlers that may lay the
        // comparison again.
        let keep = self.keep.get().filter(|_| !unsettled || exhausted);
        drop(rows);
        match keep {
            Some(Keep::FirstHunk) => self.reveal_first_hunk(),
            // Held all along, and the rows above the run have not moved.
            Some(Keep::Scroll(_)) => self.keep.set(None),
            Some(Keep::Line(column, n, at)) => {
                self.keep.set(None);
                let row = self.rows.borrow().lines[column]
                    .iter()
                    .position(|&line| line == Some(n));
                if let Some(row) = row {
                    self.reveal(row, f64::from(at));
                }
            }
            None => {}
        }
    }
}
