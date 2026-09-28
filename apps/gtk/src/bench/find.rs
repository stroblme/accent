//! The find bar's own drill: what Ctrl+F and Ctrl+H leave selected in a bar that is already open.

use super::*;
use crate::find::{Bar, Mode};
use std::collections::VecDeque;

/// The query box's `search-changed` delay (150 ms) plus the search it starts.
const SETTLED: Duration = Duration::from_millis(300);

/// One thing to do to the bar, printed as it lands; the step after it prints what settled.
type Step = Box<dyn Fn(&Rc<App>, &Rc<Bar>)>;

/// `ACCENT_BENCH_FIND=<rel_note>` opens a note, uses two queries so the recall list has something
/// to walk, and then presses Ctrl+F over the open bar twice: once on the query that was typed,
/// once on the older one Up recalled. Last it presses Ctrl+H, which selects the replacement
/// instead. Each is printed as it lands and again once the box's delayed `search-changed` has run,
/// because the regression this covers was a select-all that came back undone a moment later.
///
/// The box the press selects must read `sel=Some((0, n))` — the whole of it — in both lines of
/// all three cases.
pub(super) fn bench_find(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::timeout_add_local_once(Duration::from_millis(600), move || {
        let bar = app.pane().find.clone();
        let steps: Vec<Step> = vec![
            Box::new(|_, bar| {
                bar.open(Mode::Find);
                // Two queries used, newest last: `remember` is what Up walks back through.
                for query in ["alpha", "beta"] {
                    typed(bar, query);
                    bar.step(true);
                }
                press(bar, Mode::Find, "typed");
            }),
            Box::new(|_, bar| {
                state("typed_settled", bar);
                // What Up does to the box: the older query, caret at the end. The walk itself is
                // a key press on a controller no drill can fire, and this is all of it the bar
                // sees.
                typed(bar, "alpha");
            }),
            Box::new(|_, bar| press(bar, Mode::Find, "recalled")),
            Box::new(move |app, bar| {
                state("recalled_settled", bar);
                // Ctrl+H over a word the reader selected: the replacement box is the one that
                // takes the keyboard, and its select-all is in the way of the same step.
                if let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) {
                    let word = tab.buffer.iter_at_offset(8);
                    tab.buffer
                        .select_range(&word, &tab.buffer.iter_at_offset(13));
                }
                bar.replace_box().set_text("gamma");
                press(bar, Mode::Replace, "replace");
            }),
            Box::new(|_, bar| state("replace_settled", bar)),
        ];
        run(app, bar, steps.into());
    });
}

/// Run each step `SETTLED` after the one before, so every step sees what the last one left once
/// the box's delayed search has run, and quit after the last.
fn run(app: Rc<App>, bar: Rc<Bar>, mut steps: VecDeque<Step>) {
    let Some(step) = steps.pop_front() else {
        return bench_quit(&app);
    };
    step(&app, &bar);
    glib::timeout_add_local_once(SETTLED, move || run(app, bar, steps));
}

/// Open the bar in `mode`, as the accelerator does, and print what that left selected.
fn press(bar: &Rc<Bar>, mode: Mode, label: &str) {
    bar.open(mode);
    bench_pump();
    state(label, bar);
}

/// Put `text` in the query box the way typing and the recall walk both leave it.
fn typed(bar: &Rc<Bar>, text: &str) {
    bar.query_box().set_text(text);
    bar.query_box().set_position(-1);
}

fn state(label: &str, bar: &Rc<Bar>) {
    println!(
        "bench find case={label} query={:?} sel={:?} replace={:?} sel={:?}",
        bar.query_box().text(),
        bar.query_box().selection_bounds(),
        bar.replace_box().text(),
        bar.replace_box().selection_bounds(),
    );
}
