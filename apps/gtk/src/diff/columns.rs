//! Shared scrolling and row alignment for any number of comparison columns.
//!
//! [`super::Compare`] supplies [`Rows`]; views supply measurements. Padding aligns rows,
//! connector strips and overlay buttons follow them, and relayout preserves the reading line.

use adw::prelude::*;
use gtk::{gdk, glib};
use sourceview5::prelude::*;
use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::{Rc, Weak};

use super::Pane;
use super::heights::{Height, Known, listen};
use super::links::Links;
use super::pad::{
    UNMEASURED, carried, carried_all, is_pad, measure, one_line, pad, padding, reclaim, unmeasured,
};
use crate::editor;

/// Initial first-hunk viewport fraction: one quarter down, leaving
/// preceding context visible.
const FIRST_HUNK_AT: f64 = 0.25;

/// Blank space a hidden run leaves behind, for the button that opens it to sit in.
const GAP_PX: i32 = 28;
/// Retry count and interval for heights GTK has not validated. GTK
/// validates a screenful per idle, so a few retries cover ordinary notes.
const SETTLE: u8 = 10;
const SETTLE_AFTER: std::time::Duration = std::time::Duration::from_millis(100);
/// Minimum measurement/padding time per pass before yielding to idle
/// ([`Columns::relayout`]), avoiding multiple misaligned frames when
/// opening a few hundred lines on a busy machine.
const SLICE: std::time::Duration = std::time::Duration::from_millis(50);

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
    /// Column `.0` line `.1` text at viewport offset `.2`, using GTK positions.
    /// Anchor reader or caret text ([`Columns::let_go`], [`Columns::caret_line`])
    /// so changed padding above cannot move it. Release after rows settle
    /// instead of restoring from the grid.
    Read(usize, usize, i32),
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
    /// The rows hidden right now, each run with the key its buttons open it by: one in every
    /// column, centred in the blank space the run leaves.
    pub(super) hidden: Vec<(Range<usize>, usize)>,
    /// The first hunk's first row, where a comparison opens.
    pub(super) first: Option<usize>,
}

/// Measured natural row heights per column (`None` for absent/hidden lines),
/// hidden-button space and shared-grid row starts.
#[derive(Default)]
pub(super) struct Grid {
    #[cfg(feature = "bench")]
    pub(super) heights: Vec<Vec<Option<i32>>>,
    #[cfg(feature = "bench")]
    pub(super) extra: Vec<i32>,
    pub(super) tops: Vec<i32>,
    /// Where the last row ends.
    pub(super) end: i32,
}

/// Relayout counters since [`Columns::passes`]: pass causes, measurement
/// sources ([`super::pad::MEASURED`]), repadded lines, whole-column fallbacks
/// and padding-read/measure/pad/longest-pass milliseconds.
#[cfg(feature = "bench")]
#[derive(Default)]
pub(super) struct Passes {
    asked: Vec<(&'static str, u32)>,
    /// What asked for the pass to come.
    why: &'static str,
    repadded: u64,
    recounted: u64,
    ms: [f64; 4],
}

pub(super) struct Columns {
    weak: Weak<Columns>,
    pub(super) panes: Vec<Pane>,
    /// Which column is the user's own editor, if any. Its text is read, never set.
    pub(super) editable: Option<usize>,
    pub(super) paned: gtk::Paned,
    /// The strips between the columns, over the paned.
    pub(super) links: Links,
    /// What the host laid last.
    pub(super) rows: RefCell<Rows>,
    /// The grid the last relayout laid down, kept for the bench's checks.
    pub(super) grid: RefCell<Grid>,
    /// Per column, its lines' heights as the last passes left them: see [`Columns::relayout`].
    known: Vec<Rc<Known>>,
    pub(super) pending: RefCell<Option<glib::SourceId>>,
    /// Remaining retries for estimated GTK heights, reset by refresh. Bound
    /// them so lines never validated cannot cause infinite relayout.
    settling: Cell<u8>,
    /// Whether the host's rows are waiting for a diff of texts changed since: see
    /// [`Columns::wait`].
    waiting: Cell<bool>,
    /// Original per-column adjustments, replaced by the first column's
    /// shared adjustment and restored by [`Columns::leave`].
    own: Vec<gtk::Adjustment>,
    /// The bottom margin each view was last given here, and how much of it is the blank under a
    /// column with no line: see [`Columns::page_bottom`].
    bottoms: Vec<Cell<(i32, i32)>>,
    /// Reading position held until relayout completes or [`let_go_on_input`] detects scrolling.
    ///
    /// Initially hold the first hunk so comparisons open on changes. Afterwards preserve the
    /// line or scroll selected by opened runs and reader input.
    pub(super) keep: Cell<Option<Keep>>,
    /// Whether GTK is animating and moved scroll this frame. Animated scroll
    /// moves a held line instead of being undone ([`Columns::hold`]).
    ticking: Cell<bool>,
    stepped: Cell<bool>,
    /// The frame clock's handlers that hold a kept line, while one is kept.
    hooks: Rc<RefCell<Vec<glib::SignalHandlerId>>>,
    handlers: RefCell<Vec<(glib::Object, glib::SignalHandlerId)>>,
    /// Where in the grid the buttons were last laid for (see [`Columns::lay_buttons`]), and
    /// whether they are waiting to be laid again for where the view has moved since.
    laid_for: Cell<(i32, i32)>,
    following: Cell<bool>,
    #[cfg(feature = "bench")]
    pub(super) passes: RefCell<Passes>,
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

/// Swap `scroller` to `adjustment` with overlay scrolling temporarily off.
/// GTK 4.22 otherwise leaves its fade handler on the old adjustment,
/// which can access freed memory after the scroller is disposed.
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

/// Button grid interval for viewport `y`/`height`, extended one page
/// above and below to avoid relaying on small scrolls.
fn near(y: i32, height: i32) -> (i32, i32) {
    (y - height, y + 2 * height)
}

/// The items of `items`, sorted down the page, that reach into `lo..hi`, `span` saying where each
/// starts and ends.
pub(super) fn reaching<T>(
    items: &[T],
    span: impl Fn(&T) -> (i32, i32),
    lo: i32,
    hi: i32,
) -> Range<usize> {
    let first = items.partition_point(|item| span(item).1 <= lo);
    first..items.partition_point(|item| span(item).0 < hi).max(first)
}

/// Release [`Columns::keep`] on reader scroll input and hold the resulting top line instead.
///
/// Capture wheel, paging, Home/End and scrollbar/minimap presses before movement. Hold after
/// the wheel, validated key scroll or press release; Home/End use the caret. Programmatic
/// adjustment changes emit none of these inputs, so they cannot cancel their own hold.
fn let_go_on_input(root: &gtk::Widget, columns: &Weak<Columns>) {
    let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    wheel.set_propagation_phase(gtk::PropagationPhase::Capture);
    let w = columns.clone();
    wheel.connect_scroll(move |wheel, _, _| {
        // With Control the wheel zooms: see `zoom::zoom_on_wheel`.
        let zoom = wheel
            .current_event_state()
            .contains(gdk::ModifierType::CONTROL_MASK);
        if let Some(c) = w.upgrade().filter(|_| !zoom) {
            c.let_go(Some(glib::Priority::HIGH), false);
        }
        glib::Propagation::Proceed
    });
    root.add_controller(wheel);
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let w = columns.clone();
    keys.connect_key_pressed(move |_, key, _, state| {
        if let Some(c) = w.upgrade().filter(|_| scrolls(key, state)) {
            let resize = glib::Priority::from(gtk::PRIORITY_RESIZE as i32);
            // Control is down for Ctrl+Home or Ctrl+End alone, which go to the caret.
            let caret = state.contains(gdk::ModifierType::CONTROL_MASK);
            c.let_go(Some(resize), caret);
        }
        glib::Propagation::Proceed
    });
    root.add_controller(keys);
    // A drag on the scrollbar or the minimap claims the press, so its release is heard as an
    // event rather than by a gesture.
    let dragged = Rc::new(Cell::new(false));
    let press = gtk::GestureClick::new();
    press.set_propagation_phase(gtk::PropagationPhase::Capture);
    let (w, drag) = (columns.clone(), dragged.clone());
    press.connect_pressed(move |press, _, x, y| {
        let bar = |w: gtk::Widget| {
            w.ancestor(gtk::Scrollbar::static_type())
                .or_else(|| w.ancestor(crate::minimap::Minimap::static_type()))
                .is_some()
        };
        let picked = press
            .widget()
            .and_then(|w| w.pick(x, y, gtk::PickFlags::DEFAULT));
        if let Some(c) = w.upgrade().filter(|_| picked.is_some_and(bar)) {
            drag.set(c.let_go(None, false));
        }
    });
    root.add_controller(press);
    let release = gtk::EventControllerLegacy::new();
    release.set_propagation_phase(gtk::PropagationPhase::Capture);
    let w = columns.clone();
    release.connect_event(move |_, event| {
        if event.event_type() == gdk::EventType::ButtonRelease
            && dragged.take()
            && let Some(c) = w.upgrade()
        {
            c.hold_after(glib::Priority::HIGH, false);
        }
        glib::Propagation::Proceed
    });
    root.add_controller(release);
}

/// Recognize Page Up/Down without Ctrl and Ctrl+Home/End as reader scroll.
/// Ctrl+Page switches tabs; caret-edge arrow scrolls are not handled here.
fn scrolls(key: gdk::Key, state: gdk::ModifierType) -> bool {
    let control = state.contains(gdk::ModifierType::CONTROL_MASK);
    match key {
        gdk::Key::Page_Up | gdk::Key::Page_Down | gdk::Key::KP_Page_Up | gdk::Key::KP_Page_Down => {
            !control
        }
        gdk::Key::Home | gdk::Key::End | gdk::Key::KP_Home | gdk::Key::KP_End => control,
        _ => false,
    }
}

impl Columns {
    /// `panes` side by side, the first on the left; `editable` names the one whose buffer is the
    /// user's.
    pub(super) fn new(mut panes: Vec<Pane>, editable: Option<usize>) -> Rc<Self> {
        // First, so the roots below are the columns with their room for the strips.
        let links = Links::new(&mut panes);
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
            links,
            rows: RefCell::new(Rows::default()),
            grid: RefCell::new(Grid::default()),
            known: (0..n).map(|_| Rc::default()).collect(),
            pending: RefCell::new(None),
            settling: Cell::new(0),
            waiting: Cell::new(false),
            own,
            bottoms: (0..n).map(|_| Cell::default()).collect(),
            keep: Cell::new(Some(Keep::FirstHunk)),
            ticking: Cell::new(false),
            stepped: Cell::new(false),
            hooks: Rc::default(),
            handlers: RefCell::new(Vec::new()),
            laid_for: Cell::new((0, 0)),
            following: Cell::new(false),
            #[cfg(feature = "bench")]
            passes: RefCell::new(Passes {
                why: "idle",
                ..Passes::default()
            }),
        });

        // Weak throughout: every handler below is connected to something the comparison owns,
        // and a strong capture is a cycle.
        let weak = this.weak.clone();
        let connect = |object: glib::Object, id: glib::SignalHandlerId| {
            this.handlers.borrow_mut().push((object, id));
        };
        this.links.overlay.set_child(Some(&this.paned));
        this.links.follow(weak.clone());
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
                c.ask("alloc");
                c.schedule_relayout();
            });
            connect(hadj.upcast(), id);
            let_go_on_input(&pane.root, &weak);
        }
        // GTK lays lines out lazily and the total height moves as it reaches them, as it does
        // on a font change; the rows are re-measured each time it settles.
        let w = weak.clone();
        let vadj = this.own[0].clone();
        let id = vadj.connect_upper_notify(move |_| {
            if let Some(c) = w.upgrade() {
                c.ask("upper");
                c.schedule_relayout();
            }
        });
        connect(vadj.clone().upcast(), id);
        // The strips follow the rows on screen, and the buttons the view.
        let w = weak.clone();
        let id = vadj.connect_value_changed(move |_| {
            if let Some(c) = w.upgrade() {
                c.links.relaid();
                c.follow_view();
            }
        });
        connect(vadj.clone().upcast(), id);
        let w = weak.clone();
        let id = vadj.connect_page_size_notify(move |_| {
            if let Some(c) = w.upgrade() {
                c.follow_view();
            }
        });
        connect(vadj.clone().upcast(), id);
        // A scroll held by `Columns::hold_scroll`, or a line kept by `Compare::set_side`, goes
        // back to where it is held.
        let w = weak.clone();
        let id = vadj.connect_value_changed(move |adj| {
            let Some(c) = w.upgrade() else { return };
            c.stepped.set(c.stepped.get() || c.ticking.get());
            match c.keep.get() {
                Some(Keep::Scroll(value)) if adj.value() != value => adj.set_value(value),
                // An animated scroll moves the kept line along: see `Columns::hold`.
                Some(Keep::Line(column, ..) | Keep::Read(column, ..)) if c.stepped.get() => {
                    c.keep.set(Some(c.line_on_top(column)));
                }
                _ => c.hold_kept(),
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
        for (pane, known) in this.panes.iter().zip(&this.known) {
            for (object, id) in listen(&pane.buffer, known) {
                connect(object, id);
            }
        }
        this
    }

    /// The columns side by side with the strips between them, which is what the host shows.
    pub(super) fn widget(&self) -> &gtk::Widget {
        self.links.overlay.upcast_ref()
    }

    /// Lay the columns out from `rows`: the host's every refresh.
    pub(super) fn lay(&self, rows: Rows) {
        self.waiting.set(false);
        *self.rows.borrow_mut() = rows;
        self.settling.set(SETTLE);
        self.relayout(false);
    }

    /// Defer layout until the host refreshes rows after text changes, avoiding
    /// padding stale line positions or prematurely releasing viewport holds.
    pub(super) fn wait(&self) {
        self.waiting.set(true);
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

    /// Find the editor caret line and its GTK text offset while visible,
    /// or its target view edge with `edge`. Anchor text rather than row
    /// padding ([`Keep::Read`]) so changing blanks do not move it.
    pub(super) fn caret_line(&self, edge: bool) -> Option<Keep> {
        let mine = self.editable?;
        let (buffer, view) = (&self.panes[mine].buffer, &self.panes[mine].view);
        let mut line = buffer.iter_at_mark(&buffer.get_insert());
        line.set_line_offset(0);
        let (seen, text) = (view.visible_rect(), view.iter_location(&line));
        let at = match edge {
            true => (text.y() - seen.y()).clamp(0, (seen.height() - text.height()).max(0)),
            false => text.y() - seen.y(),
        };
        let n = line.line() as usize + 1;
        (0..seen.height())
            .contains(&at)
            .then_some(Keep::Read(mine, n, at))
    }

    /// Hold the top line across width changes until rows settle, preventing
    /// individual columns' rewrap corrections from fighting on a shared scroll.
    fn rewrapped(&self) {
        if self.keep.get().is_none() {
            self.hold(self.top_line(self.editable.unwrap_or(self.panes.len() - 1)));
            self.settling.set(SETTLE);
        }
    }

    /// Retain `keep` until rows settle, restoring kept lines after layout and before paint.
    ///
    /// Animated GTK scrolling moves the hold with it. [`Columns::ticking`] marks update-phase
    /// animation steps so restoring an adjustment cannot cancel an active animation.
    pub(super) fn hold(&self, keep: Option<Keep>) {
        self.keep.set(keep);
        let unhooked = self.hooks.borrow().is_empty();
        let (Some(Keep::Line(..) | Keep::Read(..)), true, Some(clock)) =
            (keep, unhooked, self.panes[0].view.frame_clock())
        else {
            return;
        };
        // The comparison while it keeps a line; once it does not, the handlers go.
        let kept = {
            let (w, ids) = (self.weak.clone(), self.hooks.clone());
            move |clock: &gdk::FrameClock| {
                let c = w.upgrade();
                if let Some(c) = c
                    .as_ref()
                    .filter(|c| matches!(c.keep.get(), Some(Keep::Line(..) | Keep::Read(..))))
                {
                    return Some(c.clone());
                }
                if let Some(c) = c {
                    c.ticking.set(false);
                    c.stepped.set(false);
                }
                for id in ids.borrow_mut().drain(..) {
                    clock.disconnect(id);
                }
                None
            }
        };
        let k = kept.clone();
        let before = clock.connect_before_paint(move |clock| {
            if let Some(c) = k(clock) {
                c.ticking.set(true);
                c.stepped.set(false);
            }
        });
        // After GTK's own handlers, which step the animations.
        let k = kept.clone();
        let update = clock.connect_local("update", true, move |args| {
            if let Some(c) = k(&args[0].get().ok()?) {
                c.ticking.set(false);
            }
            None
        });
        let layout = clock.connect_layout(move |clock| {
            if let Some(c) = kept(clock).filter(|c| !c.stepped.get()) {
                c.hold_kept();
            }
        });
        self.hooks.borrow_mut().extend([before, update, layout]);
    }

    /// Release a pending hold on reader input, then anchor the resulting
    /// top line at `after`, or the caret with `caret`. Return whether a hold
    /// existed; settled rows need no replacement hold.
    fn let_go(&self, after: Option<glib::Priority>, caret: bool) -> bool {
        let kept = self.keep.take().is_some();
        if let (true, Some(after)) = (kept, after) {
            self.hold_after(after, caret);
        }
        kept
    }

    /// After input at `after`, hold the top or caret line until rows settle
    /// ([`Columns::let_go`]). Place Home/End caret anchors at their destination
    /// immediately so lazy GTK estimates cannot leave them short of file end.
    fn hold_after(&self, after: glib::Priority, caret: bool) {
        let w = self.weak.clone();
        glib::idle_add_local_full(after, move || {
            if let Some(c) = w.upgrade() {
                let top = || c.line_on_top(c.editable.unwrap_or(c.panes.len() - 1));
                let caret = caret.then(|| c.caret_line(true)).flatten();
                c.hold(Some(caret.unwrap_or_else(top)));
                if caret.is_some() {
                    c.hold_kept();
                }
                c.settling.set(SETTLE);
                c.ask("hold");
                c.schedule_relayout();
            }
            glib::ControlFlow::Break
        });
    }

    /// Read the column's top text line and GTK viewport offset, matching
    /// where the reader left it while [`Columns::top_line`]'s grid is stale.
    fn line_on_top(&self, column: usize) -> Keep {
        let view = &self.panes[column].view;
        let seen = view.visible_rect().y();
        let line = view.line_at_y(seen).0;
        let at = view.iter_location(&line).y() - seen;
        Keep::Read(column, line.line() as usize + 1, at)
    }

    /// Return the reading line in `column` and its row's offset below the viewport top.
    ///
    /// Use the nearest row above if the top row has no line. Pending [`Keep::Line`] anchors use
    /// their updated row position so consecutive companion reads preserve the same location.
    pub(super) fn top_line(&self, column: usize) -> Option<Keep> {
        let (rows, grid) = (self.rows.borrow(), self.grid.borrow());
        let lines = rows.lines.get(column)?;
        let kept = match self.keep.get() {
            Some(Keep::Line(c, n, at)) => (rows.lines.get(c))
                .and_then(|lines| lines.iter().position(|&line| line == Some(n)))
                .and_then(|r| grid.tops.get(r))
                .map(|top| top - at),
            _ => None,
        };
        let seen = kept.unwrap_or_else(|| self.panes[0].view.visible_rect().y());
        let top = grid.tops.partition_point(|&y| y <= seen).max(1);
        let (row, n) = lines[..top.min(lines.len())]
            .iter()
            .enumerate()
            .rev()
            .find_map(|(r, line)| Some((r, (*line)?)))?;
        Some(Keep::Line(column, n, grid.tops[row] - seen))
    }

    /// Hold the kept line, if one is kept, where it is kept: see [`Columns::hold_line`].
    fn hold_kept(&self) {
        match self.keep.get() {
            Some(Keep::Line(column, n, at)) => self.hold_line(column, n, at, false),
            Some(Keep::Read(column, n, at)) => self.hold_line(column, n, at, true),
            _ => {}
        }
    }

    /// Place line `n` of `column` at viewport offset `at` using its current GTK position.
    ///
    /// Shared adjustments and lazy validation can move lines during relayout. Row anchors
    /// ([`Keep::Line`]) include padding; reader anchors ([`Keep::Read`]) use text when `text` is
    /// set, preserving the visible line even while blank space changes.
    fn hold_line(&self, column: usize, n: usize, at: i32, text: bool) {
        let pane = &self.panes[column];
        let Some(line) = pane.buffer.iter_at_line(n as i32 - 1) else {
            return;
        };
        let y = match text {
            true => pane.view.iter_location(&line).y(),
            false => pane.view.line_yrange(&line).0 + carried(&pane.view, &line).0,
        };
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

    /// Place `row` at viewport offset `at` from the measured grid, then release its hold.
    ///
    /// The shared adjustment scrolls every column. Use the relayout grid rather than lazy GTK
    /// figures, which may not include newly applied padding.
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

    /// Remove shared layout before panes separate, after host tags are cleared.
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
            pane.pool.claim(&pane.view, &[]);
        }
        self.rows.borrow_mut().hidden.clear();
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

    /// Copy editor margins, spacing and tab width to companion columns for
    /// aligned first rows. Remeasure heading markers after margin changes
    /// and, with `refont`, editor font changes. Bottom margin remains owned
    /// by [`Columns::relayout`].
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

    /// Page bottom margin excluding comparison blank space. Use the editor's
    /// margin for companions; adopt externally changed margins, such as zoom.
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

    /// Lay hidden-run and strip buttons only [`near`] the view, hiding others
    /// to avoid main-loop stalls from thousands of offscreen hunks.
    fn lay_buttons(&self) {
        let (rows, grid) = (self.rows.borrow(), self.grid.borrow());
        // Nothing laid yet.
        if grid.tops.len() != rows.changed.len() {
            return;
        }
        let seen = self.panes[0].view.visible_rect();
        let (lo, hi) = near(seen.y(), seen.height());
        let span = |(gap, _): &(Range<usize>, usize)| {
            let top = grid.tops[gap.start];
            (top, top + GAP_PX)
        };
        let runs = &rows.hidden[reaching(&rows.hidden, span, lo, hi)];
        let keyed: Vec<(usize, usize)> = runs.iter().map(|(gap, key)| (*key, gap.len())).collect();
        for pane in &self.panes {
            let width = pane.view.visible_rect().width();
            for (widget, (gap, _)) in pane.pool.claim(&pane.view, &keyed).iter().zip(runs) {
                // Buffer coordinates, which start at the first paragraph — the view's top margin
                // is outside them — and scroll with the text.
                let (_, wanted, _, _) = widget.measure(gtk::Orientation::Horizontal, -1);
                let (_, height, _, _) = widget.measure(gtk::Orientation::Vertical, -1);
                let (x, y) = (
                    (width - wanted) / 2,
                    grid.tops[gap.start] + (GAP_PX - height) / 2,
                );
                pane.view.move_overlay(widget, x.max(0), y);
            }
        }
        drop((rows, grid));
        self.laid_for.set((lo, hi));
        self.links.claim(self, lo, hi);
    }

    /// Queue an idle button relayout when scrolling within half a page of
    /// the prior interval edge, avoiding GTK layout reentrancy ([`Links::relaid`]).
    fn follow_view(&self) {
        let seen = self.panes[0].view.visible_rect();
        let ((lo, hi), half) = (self.laid_for.get(), seen.height() / 2);
        let covered = lo <= seen.y() - half && seen.y() + seen.height() + half <= hi;
        if covered || self.following.replace(true) {
            return;
        }
        let weak = self.weak.clone();
        glib::idle_add_local_once(move || {
            if let Some(c) = weak.upgrade() {
                c.following.set(false);
                c.lay_buttons();
            }
        });
    }

    /// How many buttons are laid, over the columns and in the strips.
    pub(super) fn buttons(&self) -> usize {
        self.panes.iter().map(|p| p.pool.shown()).sum::<usize>() + self.links.shown()
    }

    /// Name what asks for the next pass, for the bench's counters: the first to ask, where a pass
    /// is already on its way.
    fn ask(&self, _why: &'static str) {
        #[cfg(feature = "bench")]
        if self.pending.borrow().is_none() {
            self.passes.borrow_mut().why = _why;
        }
    }

    /// What the passes since the last call did, see [`Passes`], and nothing again from here on.
    #[cfg(feature = "bench")]
    pub(super) fn passes(&self) -> String {
        let why = self.passes.borrow().why;
        let Passes {
            asked,
            repadded,
            recounted,
            ms,
            ..
        } = self.passes.replace(Passes {
            why,
            ..Passes::default()
        });
        let measured = super::pad::MEASURED.take();
        let asked: Vec<String> = asked.iter().map(|(why, n)| format!("{why}:{n}")).collect();
        format!(
            "passes={} gtk={} own={}/{}/{} repadded={repadded} recounted={recounted} \
             ms={:.0}/{:.0}/{:.0}/{:.0}",
            asked.join(","),
            measured[0],
            measured[1],
            measured[2],
            measured[3],
            ms[0],
            ms[1],
            ms[2],
            ms[3]
        )
    }

    /// Count a pass `why` asked for that read the padding by `read`, measured by `measured`,
    /// padded by `padded`, having started at `start`.
    #[cfg(feature = "bench")]
    fn count_pass(
        &self,
        why: &'static str,
        [start, read, measured, padded]: [std::time::Instant; 4],
        repadded: usize,
    ) {
        let ms = |a: std::time::Instant, b: std::time::Instant| (b - a).as_secs_f64() * 1e3;
        let mut passes = self.passes.borrow_mut();
        match passes.asked.iter_mut().find(|(w, _)| *w == why) {
            Some((_, n)) => *n += 1,
            None => passes.asked.push((why, 1)),
        }
        passes.repadded += repadded as u64;
        passes.ms[0] += ms(start, read);
        passes.ms[1] += ms(read, measured);
        passes.ms[2] += ms(measured, padded);
        passes.ms[3] = passes.ms[3].max(ms(start, std::time::Instant::now()));
    }

    pub(super) fn schedule_relayout(&self) {
        if self.pending.borrow().is_some() {
            return;
        }
        let weak = self.weak.clone();
        let id = glib::idle_add_local_once(move || {
            if let Some(c) = weak.upgrade() {
                *c.pending.borrow_mut() = None;
                c.relayout(true);
            }
        });
        *self.pending.borrow_mut() = Some(id);
    }

    /// Measure and pad comparison rows, position buttons and preserve the reading anchor.
    ///
    /// Unchanged padding stays untouched so repeated passes settle without invalidating layout.
    /// [`Known`] caches unchanged line heights. Pending lines keep estimates until a `fresh`
    /// pass after GTK layout idles; unmeasured lines use [`measure`]. Fresh passes also compare
    /// column totals with GTK and re-read mismatches to catch missed or stale validation.
    ///
    /// Limit work to half the padding-read time or [`SLICE`], prioritising the viewport and rows
    /// above it. Deferred lines retain prior heights or [`one_line`] estimates and padding;
    /// idle passes continue after GTK's own layout work, avoiding long synchronous full-file passes.
    fn relayout(&self, fresh: bool) {
        #[cfg(feature = "bench")]
        let why = match fresh {
            true => std::mem::replace(&mut self.passes.borrow_mut().why, "idle"),
            false => "lay",
        };
        if self.waiting.get() || !self.panes.iter().all(|p| p.view.is_mapped()) {
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
        let mut hidden = vec![false; count];
        for (gap, _) in &rows.hidden {
            hidden[gap.clone()].fill(true);
        }
        let is_hidden = |r: usize| hidden[r];
        let start = std::time::Instant::now();
        // What each column's lines carry now, per row, read once for the measuring and the padding
        // below and ahead of any change: padding a line touches none of the others.
        let had: Vec<Vec<Option<(i32, i32)>>> = (self.panes.iter().enumerate())
            .map(|(c, pane)| {
                let st = &rows.starts[c];
                let at: Vec<i32> = rows.lines[c].iter().flatten().map(|&n| st[n - 1]).collect();
                let mut had = carried_all(&pane.view, &pane.buffer, &at).into_iter();
                rows.lines[c]
                    .iter()
                    .map(|line| line.and_then(|_| had.next()))
                    .collect()
            })
            .collect();
        #[cfg(feature = "bench")]
        let read = std::time::Instant::now();
        // Whether each column's figures can be used: see `Known::ready`.
        let kept: Vec<bool> = (self.panes.iter().zip(&self.known))
            .zip(&rows.starts)
            .map(|((pane, known), st)| known.ready(&pane.view, &pane.buffer, st))
            .collect();
        let mut known: Vec<_> = self.known.iter().map(|k| k.lines.borrow_mut()).collect();
        let one: Vec<i32> = self.panes.iter().map(|p| one_line(&p.view)).collect();
        let mut heights = vec![vec![None; count]; self.panes.len()];
        // Per column, whether a figure is an estimate.
        let mut guessed = vec![false; self.panes.len()];
        // Row by row, those within a page of the view as the last pass laid them first, then the
        // ones above them, which put them where they are, then the rest.
        let (near_from, near_to) = {
            let (grid, seen) = (self.grid.borrow(), self.panes[0].view.visible_rect());
            let (lo, hi) = near(seen.y(), seen.height());
            match grid.tops.len() == count {
                true => (
                    grid.tops.partition_point(|&y| y < lo).saturating_sub(1),
                    grid.tops.partition_point(|&y| y < hi),
                ),
                false => (0, 0),
            }
        };
        let order = || {
            (near_from..near_to)
                .chain(0..near_from)
                .chain(near_to..count)
        };
        // Half of it for measuring, the rest for padding, which measuring the whole of it left no
        // time to. Reading the padding is most of a pass over 120,000 padded rows: with `SLICE`
        // alone, the passes after Show All Unchanged Lines did little else, and with all of it
        // they held the main loop for twice as long.
        let budget = SLICE.max(start.elapsed() / 2);
        let until = std::time::Instant::now() + budget;
        let measured_by = until - budget / 2;
        let (mut late, mut sliced) = (false, false);
        for r in order() {
            for (c, pane) in self.panes.iter().enumerate() {
                let Some(n) = rows.lines[c][r] else {
                    continue;
                };
                let st = &rows.starts[c];
                let figure = kept[c].then(|| &mut known[c][n - 1]);
                if is_hidden(r) {
                    // Asked about again once it is shown.
                    if let Some(f) = figure {
                        *f = f.pending();
                    }
                    continue;
                }
                let Some((above, below)) = had[c][r] else {
                    continue;
                };
                let (height, estimate) = match figure.as_deref().copied() {
                    Some(Height::Laid(px)) => (px, false),
                    Some(Height::Pending(px)) if !fresh => (px, true),
                    // Left for a later pass, a figure from before or one line standing in.
                    Some(Height::Pending(px)) if late => {
                        sliced = true;
                        (px, true)
                    }
                    Some(Height::Unknown) if late => {
                        sliced = true;
                        guessed[c] = true;
                        heights[c][r] = Some(one[c]);
                        continue;
                    }
                    _ => {
                        let measured = measure(
                            &pane.view,
                            &pane.buffer,
                            st[n - 1],
                            st[n],
                            above + below,
                            fresh && kept[c],
                        );
                        late = std::time::Instant::now() > measured_by;
                        measured
                    }
                };
                if let Some(figure) = figure {
                    *figure = Height::of(height, estimate);
                }
                guessed[c] |= estimate;
                heights[c][r] = Some(height);
            }
        }
        for (c, pane) in self.panes.iter().enumerate() {
            // With the padding its lines carry, the column adds up to what GTK has laid out down
            // to its last row's line, or some figure is out of date — a change no signal told of,
            // a line GTK had not laid out since: then every line is read again, once GTK has laid
            // them out. The buffer's empty last line is no row of the diff's.
            let last = rows.lines[c].iter().rev().find_map(|line| *line);
            let bottom = last.and_then(|n| pane.buffer.iter_at_line(n as i32 - 1));
            let (Some(bottom), true) = (bottom, fresh && kept[c] && !guessed[c]) else {
                continue;
            };
            let (y, h) = pane.view.line_yrange(&bottom);
            let sum: i32 = (heights[c].iter().zip(&had[c]))
                .map(|pair| match pair {
                    (Some(h), Some((above, below))) => h + above + below,
                    _ => 0,
                })
                .sum();
            if sum != y + h {
                known[c].iter_mut().for_each(|h| *h = h.pending());
                guessed[c] = true;
                #[cfg(feature = "bench")]
                {
                    self.passes.borrow_mut().recounted += 1;
                }
            }
        }
        drop(known);
        let estimated = guessed.contains(&true);
        #[cfg(feature = "bench")]
        let measured = std::time::Instant::now();
        let mut extra = vec![0; count];
        for (gap, _) in &rows.hidden {
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

        // Every line, hidden ones included, so a row that is hidden now or was the last one before
        // an edit does not keep what it carried then; in the same order, and those not near the
        // view only while the pass has time. One left over is laid out with what it carried until
        // a later pass, the rows under it off level meanwhile, and a hidden one not at all.
        let mut repadded = 0;
        for r in order() {
            for (c, pane) in self.panes.iter().enumerate() {
                let Some(n) = rows.lines[c][r] else {
                    continue;
                };
                let (st, now) = (&rows.starts[c], &pads[c]);
                // Read ahead of any change, so only a line whose padding moves is touched.
                if had[c][r] == Some((now.above[r], now.below[r])) {
                    continue;
                }
                if !(near_from..near_to).contains(&r) && std::time::Instant::now() > until {
                    sliced = true;
                    continue;
                }
                let moved = pad(
                    &pane.view,
                    &pane.buffer,
                    st[n - 1],
                    st[n],
                    now.above[r],
                    now.below[r],
                );
                // GTK has the line to lay out again with its new padding.
                if moved && kept[c] {
                    self.known[c].touch(n - 1);
                }
                repadded += usize::from(moved);
            }
        }
        #[cfg(feature = "bench")]
        let padded = std::time::Instant::now();

        let end = tops.last().map_or(0, |top| {
            let last = |c: &Vec<Option<i32>>| c[count - 1].unwrap_or(0);
            top + heights.iter().map(last).max().unwrap_or(0) + extra[count - 1]
        });
        let moved = {
            let grid = self.grid.borrow();
            grid.tops != tops || grid.end != end
        };
        *self.grid.borrow_mut() = Grid {
            #[cfg(feature = "bench")]
            heights,
            #[cfg(feature = "bench")]
            extra,
            tops,
            end,
        };
        self.lay_buttons();
        // The strips follow the rows, and a pass that moved none leaves them be, as it leaves
        // the rows.
        if moved {
            self.links.relaid();
        }
        // Every view laid out again in the next frame, ahead of painting any. GTK lays a view out
        // from an idle of its own and repaints it once it has, so one column could reach the
        // screen a frame ahead of another: a keystroke's own line in the editor a frame after
        // the padding that answers it beside it.
        if repadded > 0 {
            for pane in &self.panes {
                pane.view.queue_allocate();
            }
        }
        // GTK had not laid some line out yet: ask again once it has. So too while the view waits
        // to be put somewhere for a pass that moved nothing: padding GTK has not laid out yet is
        // not in where the scroll puts a line, and each view keeps its top line where it was as it
        // catches up. At idle priority, below GTK's own layout idles: a pass measures every line
        // GTK has not reached in a layout of its own, 10 s of them once 120,000 rows were shown,
        // and asked again on a timer it left GTK only the moments between passes to catch up in,
        // the main loop held for minutes.
        let unsettled = estimated || sliced || (self.keep.get().is_some() && repadded > 0);
        let exhausted = self.settling.get() == 0;
        // What this pass left over goes on at once, and does not count as asking GTK again.
        if sliced {
            self.ask("slice");
            self.schedule_relayout();
        } else if unsettled && !exhausted && self.pending.borrow().is_none() {
            self.settling.set(self.settling.get() - 1);
            let weak = self.weak.clone();
            let id = glib::timeout_add_local_full(
                SETTLE_AFTER,
                glib::Priority::DEFAULT_IDLE,
                move || {
                    if let Some(c) = weak.upgrade() {
                        *c.pending.borrow_mut() = None;
                        c.ask("retry");
                        c.relayout(true);
                    }
                    glib::ControlFlow::Break
                },
            );
            *self.pending.borrow_mut() = Some(id);
        }
        // Once every row is measured and laid as the grid has it, or GTK has been asked as often
        // as it will be. After the borrows: moving the scroll runs handlers that may lay the
        // comparison again.
        let keep = self.keep.get().filter(|_| !unsettled || exhausted);
        drop(rows);
        #[cfg(feature = "bench")]
        self.count_pass(why, [start, read, measured, padded], repadded);
        match keep {
            Some(Keep::FirstHunk) => self.reveal_first_hunk(),
            // Held all along: the rows above the run have not moved, and the reader's line is
            // where GTK draws it.
            Some(Keep::Scroll(_) | Keep::Read(..)) => self.keep.set(None),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_are_laid_on_what_reaches_a_page_either_side_of_the_view() {
        assert_eq!(near(1000, 600), (400, 2200));
        // Runs 28 px tall down the page.
        let tops = [0, 100, 200, 300, 400];
        let span = |&top: &i32| (top, top + 28);
        assert_eq!(
            reaching(&tops, span, 110, 310),
            1..4,
            "partly inside counts"
        );
        assert_eq!(reaching(&tops, span, 128, 300), 2..3, "touching does not");
        assert_eq!(reaching(&tops, span, 500, 900), 5..5);
        assert_eq!(reaching(&[] as &[i32], span, 0, 900), 0..0);
    }

    #[test]
    fn page_keys_and_control_home_or_end_are_the_reader_scrolling() {
        let (plain, control) = (gdk::ModifierType::empty(), gdk::ModifierType::CONTROL_MASK);
        assert!(scrolls(gdk::Key::Page_Down, plain));
        assert!(scrolls(gdk::Key::KP_Page_Up, gdk::ModifierType::SHIFT_MASK));
        assert!(!scrolls(gdk::Key::Page_Up, control), "a tab switch");
        assert!(scrolls(gdk::Key::End, control));
        assert!(scrolls(gdk::Key::KP_Home, control));
        assert!(!scrolls(gdk::Key::Home, plain), "the start of the line");
        assert!(!scrolls(gdk::Key::Down, plain));
    }
}
