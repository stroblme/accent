//! Find-bar drills for repeated shortcuts, selections and query toggles.

use super::*;
use crate::find::{Bar, Mode};
use std::collections::VecDeque;

/// The query box's `search-changed` delay (150 ms) plus the search it starts.
const SETTLED: Duration = Duration::from_millis(300);

/// One thing to do to the bar, printed as it lands; the step after it prints what settled.
type Step = Box<dyn Fn(&Rc<App>, &Rc<Bar>)>;

/// `ACCENT_BENCH_FIND=<rel_note>` tests Ctrl+F on a typed and a recalled query, then Ctrl+H
/// on the replacement box. Print selections immediately and after delayed `search-changed`:
/// the regression was a select-all undone by that signal.
///
/// All three cases must report `sel=Some((0, n))` both times.
///
/// `ACCENT_BENCH_FIND=options[:<rel_pdf>]` is [`bench_find_options`] instead.
pub(super) fn bench_find(app: &Rc<App>, rel: &str) {
    if let Some(pdf) = rel.strip_prefix("options") {
        return bench_find_options(app, pdf.strip_prefix(':'));
    }
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

/// The note [`bench_find_options`] writes into the vault and takes away again.
const NOTE: &str = "accent-bench-find.md";
/// Words the three toggles tell apart, and two calls for the regex to rewrite.
const TEXT: &str = "Foo foo food foo_bar\nÄ foo(1) FOO(22)\n";

/// `ACCENT_BENCH_FIND=options` writes a note, toggles find options and prints counts and
/// invalid-state flags. Expected counts for `foo`: `plain` 6, `case` 4, `word` 4 (excluding
/// `food` and `foo_bar`), `case_word` 2; regex `fo+\(` finds 2. `foo(` reports `Invalid pattern`
/// and `invalid=true`. Replace All of `(\w+)\((\d+)\)` with `$2-$1` produces
/// `Ä 1-foo 22-FOO` on the second line.
///
/// In rendered preview, regex is unavailable. For `foo`, expect `preview_plain` 6,
/// `preview_case` 4, `preview_word` 6 and `preview_case_word` 4; Whole Word matches word
/// starts here. For `oo`, expect 6 plain, 5 by case and 0 at word starts.
///
/// With the generated vault's `Attachments/pages.pdf`, Whole Word instead matches entire
/// words: `page` finds 5, or 0 by case; `Page` finds 5 by case; `Pag` finds 5, or 0 as a
/// whole word. Each initially shows `1 of 5`. [`stepping`] writes the PDF, so use a scratch
/// vault: `pdf_typed` page 3 `3 of 5`, `pdf_enter` page 4 `4 of 5`, `pdf_shift_enter` page 3
/// `3 of 5`, `pdf_wrapped` page 2 `1 of 1`, `pdf_edited` page 5 `5 matches`, and
/// `pdf_edited_enter` page 5 `5 of 5`. The drill closes and removes its note.
fn bench_find_options(app: &Rc<App>, pdf: Option<&str>) {
    let path = app.root().join(NOTE);
    if let Err(e) = std::fs::write(&path, TEXT) {
        println!("bench find options wrote=false {e}");
        return bench_quit(app);
    }
    app.open_path(NOTE);
    let (app, pdf) = (app.clone(), pdf.map(str::to_string));
    glib::timeout_add_local_once(Duration::from_millis(600), move || {
        let bar = app.pane().find.clone();
        let set = |bar: &Rc<Bar>, case: bool, word: bool, regex: bool| {
            for (button, on) in bar.toggles().iter().zip([case, word, regex]) {
                button.set_active(on);
            }
        };
        let steps: Vec<Step> = vec![
            Box::new(|_, bar| {
                bar.open(Mode::Find);
                typed(bar, "foo");
            }),
            Box::new(|_, bar| readout("plain", bar)),
            Box::new(move |_, bar| {
                set(bar, true, false, false);
                readout("case", bar);
                set(bar, false, true, false);
                readout("word", bar);
                set(bar, true, true, false);
                readout("case_word", bar);
                set(bar, false, false, true);
                typed(bar, r"fo+\(");
            }),
            Box::new(|_, bar| {
                readout("regex", bar);
                typed(bar, "foo(");
            }),
            Box::new(|_, bar| {
                readout("invalid", bar);
                typed(bar, r"(\w+)\((\d+)\)");
            }),
            Box::new(|app, bar| {
                bar.replace_box().set_text("$2-$1");
                bar.press_replace_all();
                let text = app.active().map(|tab| tab.text()).unwrap_or_default();
                println!("bench find options case=replaced text={text:?}");
                app.set_presenting(true);
                bar.open(Mode::Find);
            }),
            // WebKit's first page load, which a find waits for.
            Box::new(|_, _| {}),
            Box::new(|_, _| {}),
            Box::new(|_, bar| toggles("preview", bar)),
        ];
        let mut steps: VecDeque<Step> = steps.into();
        walk(
            &mut steps,
            "preview",
            &[
                ("plain", "foo", false, false),
                ("case", "foo", true, false),
                ("word", "foo", false, true),
                ("case_word", "foo", true, true),
                ("plain", "oo", false, false),
                ("case", "oo", true, false),
                ("word", "oo", false, true),
            ],
        );
        steps.push_back(Box::new(|app, _| {
            app.set_presenting(false);
            if let Some(tab) = app.active() {
                app.close_page(&tab.page);
            }
            let _ = std::fs::remove_file(app.root().join(NOTE));
        }));
        if let Some(pdf) = pdf {
            steps.push_back(Box::new(move |app, bar| {
                app.open_path(&pdf);
                bar.open(Mode::Find);
            }));
            steps.push_back(Box::new(|_, _| {}));
            steps.push_back(Box::new(|_, bar| toggles("pdf", bar)));
            walk(
                &mut steps,
                "pdf",
                &[
                    ("plain", "page", false, false),
                    ("case", "page", true, false),
                    ("case", "Page", true, false),
                    ("plain", "Pag", false, false),
                    ("word", "Pag", false, true),
                ],
            );
            steps.extend(stepping());
        }
        run(app, bar, steps);
    });
}

/// The PDF's steps from the page being read: typed on the third page, the query shows its hit,
/// Enter and Shift+Enter pressed for real (XTEST) step on and back, a query found only on the
/// second page wraps round to it, and a page edit made while reading the last page searches
/// again without moving the reader, Enter then showing that page's hit.
fn stepping() -> Vec<Step> {
    let pdf = |app: &Rc<App>| app.pdf_of(&app.pane());
    vec![
        // A step of its own, its search showing a hit as it lands.
        Box::new(|_, bar| {
            for button in bar.toggles() {
                button.set_active(false);
            }
        }),
        Box::new(move |app, _| {
            if let Some(pdf) = pdf(app) {
                pdf.goto_page(2);
            }
        }),
        Box::new(|_, bar| typed(bar, "Page")),
        Box::new(|app, bar| {
            reading("pdf_typed", app, bar);
            glib::spawn_future_local(super::git::xtest("move 700 450; focus; key Return"));
        }),
        Box::new(|_, _| {}),
        Box::new(|app, bar| {
            reading("pdf_enter", app, bar);
            glib::spawn_future_local(super::git::xtest("key shift+Return"));
        }),
        Box::new(|_, _| {}),
        // A hit only before the page being read, found by wrapping round.
        Box::new(|app, bar| {
            reading("pdf_shift_enter", app, bar);
            typed(bar, "Page 2");
        }),
        Box::new(|app, bar| {
            reading("pdf_wrapped", app, bar);
            typed(bar, "Page");
        }),
        Box::new(move |app, _| {
            if let Some(pdf) = pdf(app) {
                pdf.goto_page(4);
            }
        }),
        // The first two pages swapped, which leaves the page being read where it is.
        Box::new(move |app, _| {
            if let Some(pdf) = pdf(app) {
                pdf.edit_pages(accent_core::pdf::PageEdit::Move { from: 0, to: 1 });
            }
        }),
        Box::new(|_, _| {}),
        Box::new(|app, bar| {
            reading("pdf_edited", app, bar);
            glib::spawn_future_local(super::git::xtest("key Return"));
        }),
        Box::new(|_, _| {}),
        Box::new(|app, bar| reading("pdf_edited_enter", app, bar)),
    ]
}

/// The readout and the page being read, 1-based.
fn reading(label: &str, app: &Rc<App>, bar: &Rc<Bar>) {
    let page = app
        .pdf_of(&app.pane())
        .map_or(0, |pdf| pdf.current_page() + 1);
    println!(
        "bench find options case={label} page={page} readout={:?}",
        bar.readout().0
    );
}

/// For each of `walk`, a step setting the toggles to Match Case and Match Whole Word as it says
/// (Regular Expression off) and typing its query, and one printing the readout as `<place>_<label>`.
fn walk(
    steps: &mut VecDeque<Step>,
    place: &'static str,
    walk: &[(&'static str, &'static str, bool, bool)],
) {
    for &(label, query, case, word) in walk {
        steps.push_back(Box::new(move |_, bar| {
            for (button, on) in bar.toggles().iter().zip([case, word, false]) {
                button.set_active(on);
            }
            typed(bar, query);
        }));
        steps.push_back(Box::new(move |_, bar| {
            readout(&format!("{place}_{label}"), bar)
        }));
    }
}

/// Which toggles take a click over `place`, and what Match Whole Word says it does there.
fn toggles(place: &str, bar: &Rc<Bar>) {
    let [case, word, regex] = bar.toggles();
    println!(
        "bench find options case={place} toggles case={} word={} regex={} word_tooltip={:?}",
        case.is_sensitive(),
        word.is_sensitive(),
        regex.is_sensitive(),
        word.tooltip_text().unwrap_or_default()
    );
}

/// What the bar reads out for the query in its box.
fn readout(label: &str, bar: &Rc<Bar>) {
    let (readout, invalid) = bar.readout();
    println!(
        "bench find options case={label} query={:?} readout={readout:?} invalid={invalid}",
        bar.query_box().text()
    );
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
