//! Drills over comparisons: a note against its disk copy, two blobs, and the working tree.
//! Those driven by a note's comparison with its disk copy, two blobs among them, are in `disk`,
//! those driven by the Git pane's rows in `git`, and what both use is here.

use super::diagnostics::diagnostic;
use super::*;
use accent_api::{Fold, Severity};

mod disk;
mod git;

pub(super) use disk::{
    bench_compare, bench_compare_conflict, bench_compare_diag, bench_compare_folds,
    bench_compare_gap, bench_compare_gutter, bench_compare_left, bench_compare_page,
    bench_compare_press, bench_compare_runaway, bench_compare_unfold,
};
pub(super) use git::{
    bench_compare_clicks, bench_compare_lines, bench_compare_pads, bench_compare_pick,
    bench_compare_row, bench_compare_stale, bench_compare_typing,
};

/// What the toast over the window reads, whatever it says: [`bench_said`] looks for a failure.
pub(super) fn bench_toast(app: &Rc<App>) -> Option<String> {
    let toast = find_widget(app.window.upcast_ref(), &|w| {
        w.type_().name() == "AdwToastWidget"
    })?;
    let label = find_widget(&toast, &|w| w.is::<gtk::Label>())?;
    Some(label.downcast::<gtk::Label>().ok()?.label().to_string())
}

/// Type one character into the comparing editor's side — `line`, or the first change where it is
/// `None` — `at` of the way along that line, and print what moved while the comparison caught up:
/// how often the shared scroll range changed, whether the scroll position did, how many rows were
/// off right after the keystroke and once it had settled, and how often something laid over the
/// editor was hidden or shown. The claim is 0, false, 0, 0, 0. `padded` says whether the line
/// carried alignment padding, which is the case the flash was about, `padded_after` whether it
/// still carries it on the character just typed — the invariant the flash is the loss of — and
/// `scroll` is the position against the furthest it can go, which says whether it had room to
/// drift.
fn bench_compare_type(tab: &Rc<Tab>, at: f64, line: Option<i32>, then: impl FnOnce() + 'static) {
    let tab = tab.clone();
    glib::timeout_add_local_once(Duration::from_millis(300), move || {
        let (Some(compare), Some(adj)) = (tab.comparison(), tab.view.vadjustment()) else {
            return then();
        };
        let found = match line {
            Some(n) => tab.buffer.iter_at_line(n),
            None => compare.opens_at().map(|o| tab.buffer.iter_at_offset(o)),
        };
        let Some(line) = found else {
            println!("bench compare_type at={at} no_change");
            return then();
        };
        let padded = is_padded(&tab, &line);
        let mut end = line;
        end.forward_to_line_end();
        let offset = line.offset() + ((end.offset() - line.offset()) as f64 * at) as i32;
        // Kept as a number: the insert below invalidates every iter into the buffer.
        let number = line.line();
        let mark = tab.buffer.create_mark(None, &line, true);
        tab.view.scroll_to_mark(&mark, 0.0, true, 0.0, 0.5);
        tab.buffer.delete_mark(&mark);
        glib::timeout_add_local_once(Duration::from_millis(300), move || {
            let (value, moves, flips) = (adj.value(), Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
            let count = |n: &Rc<Cell<u32>>| {
                let n = n.clone();
                move || n.set(n.get() + 1)
            };
            // A change of the range, not a notify: GTK re-sets an unchanged one at the bottom.
            let (tick, upper) = (count(&moves), Cell::new(adj.upper()));
            let mut ids = vec![(
                adj.clone().upcast::<glib::Object>(),
                adj.connect_upper_notify(move |a| {
                    if upper.replace(a.upper()) != a.upper() {
                        tick();
                    }
                }),
            )];
            // The overlaid buttons sit one level down, on the view's text child.
            let mut stack = vec![tab.view.clone().upcast::<gtk::Widget>()];
            while let Some(widget) = stack.pop() {
                let mut child = widget.first_child();
                while let Some(c) = child {
                    let tick = count(&flips);
                    ids.push((
                        c.clone().upcast(),
                        c.connect_visible_notify(move |_| tick()),
                    ));
                    child = c.next_sibling();
                    stack.push(c);
                }
            }
            // Through the caret, as a keystroke goes in: `diff::reclaim` reads it to know which
            // line was typed into.
            tab.buffer.place_cursor(&tab.buffer.iter_at_offset(offset));
            tab.buffer.insert_at_cursor("x");
            // Read back where the character went in, which is where GTK reads the line's spacing
            // from: a tag that began at the line's first character is behind it now.
            let padded_after = is_padded(&tab, &tab.buffer.iter_at_offset(offset));
            let now = compare.misaligned();
            glib::timeout_add_local_once(Duration::from_millis(500), move || {
                println!(
                    "bench compare_type at={at} line={number} padded={padded} padded_after={padded_after} upper_moves={} value_moved={} misaligned_now={now} misaligned={} flips={} scroll={value}/{}",
                    moves.get(),
                    adj.value() != value,
                    compare.misaligned(),
                    flips.get(),
                    adj.upper() - adj.page_size(),
                );
                for (object, id) in ids {
                    object.disconnect(id);
                }
                then();
            });
        });
    });
}

/// Whether the paragraph at `at` carries alignment padding above or below it, which is what GTK
/// lays the line out with and what a character typed at the line's start must not take away.
fn is_padded(tab: &Rc<Tab>, at: &gtk::TextIter) -> bool {
    let (above, below) = (tab.view.pixels_above_lines(), tab.view.pixels_below_lines());
    at.tags().iter().any(|t| {
        (t.is_pixels_above_lines_set() && t.pixels_above_lines() > above)
            || (t.is_pixels_below_lines_set() && t.pixels_below_lines() > below)
    })
}

/// The view of one pane of a comparison: the left one, or the right with `end`.
pub(super) fn pane_view(paned: &gtk::Widget, end: bool) -> Option<gtk::TextView> {
    let paned = paned.downcast_ref::<gtk::Paned>()?;
    let pane = match end {
        true => paned.end_child(),
        false => paned.start_child(),
    }?;
    find_widget(&pane, &|w| w.is::<gtk::TextView>())?
        .downcast()
        .ok()
}

fn bench_compare_line(compare: &diff::Compare) -> String {
    let (rows, hunks, hidden, buttons) = compare.counts();
    format!(
        "rows={rows} hunks={hunks} hidden={hidden} buttons={buttons} misaligned={} skew={}",
        compare.misaligned(),
        compare.skew()
    )
}
