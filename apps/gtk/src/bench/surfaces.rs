//! One editor experience on every surface: the same keys asked of each text view a user edits or
//! reads in, and what each answered.

use super::*;

/// Down, End, a column's Down and Ctrl+C on every editor surface: a code tab and a note tab, both
/// columns of a comparison, both panes of a staged change's tab, and a notebook's code and
/// markdown cells. `rels` is `<code>,<note>,<ipynb>`, written by the drill into a scratch vault.
///
/// Each surface holds a short line, a line wrapping over several rows and short lines after it,
/// and prints `bench surface <name> down=<line>:<offset>:<row> end=… col=[…]->[…] scrolled=<bool>
/// copy=<text>`: Down and End from the third character of the wrapped line, Add Caret Below from
/// the line above it and Down at both carets, whether the first Down scrolled the view, and Ctrl+C
/// with nothing selected on the line after it. The keys are real, through XTEST, so the window's
/// accelerators, the view's controllers and GTK's bindings answer them as they answer a user.
pub(super) fn bench_surfaces(app: &Rc<App>, rels: &str) {
    scratch_only(app, "ACCENT_BENCH_KEYS=surfaces");
    let rels: Vec<String> = rels.split(',').map(str::to_string).collect();
    let [code, note, ipynb] = &rels[..] else {
        eprintln!("ACCENT_BENCH_KEYS=surfaces:<code>,<note>,<ipynb>");
        return bench_quit(app);
    };
    let (code_text, note_text) = (fixture(&code_line()), fixture(&"wide ".repeat(80)));
    let cells = notebook_json(&"wide ".repeat(80), &code_line());
    for (rel, text) in [(code, &code_text), (note, &note_text), (ipynb, &cells)] {
        if let Err(e) = std::fs::write(app.root().join(rel), text) {
            eprintln!("cannot write {rel}: {e}");
            return bench_quit(app);
        }
    }
    let (app, code, note, ipynb) = (app.clone(), code.clone(), note.clone(), ipynb.clone());
    glib::spawn_future_local(async move {
        super::git::xtest("move 400 300; focus").await;
        let Some(tab) = opened_tab(&app, &code).await else {
            return bench_quit(&app);
        };
        probe("tab_code", &tab.view).await;
        // Line numbers off by the preference, which a comparison shows on both columns anyway.
        tab.set_line_numbers(false);
        let compare = tab.compare(
            "Mine",
            ("Theirs", &other(&code_text)),
            diff::Side::New,
            false,
            None,
            "Compare",
        );
        settle().await;
        probe("compare_editor", &tab.view).await;
        let side = compare.view(diff::Side::Old).clone();
        probe("compare_side", &side).await;
        // Alt+Z with the keyboard in the read-only side unwraps both columns.
        press(&side, "alt+z").await;
        let numbers = tab.line_numbers().0;
        let unwrapped = [&tab.view, &side].map(|v| v.wrap_mode() == gtk::WrapMode::None);
        press(&side, "alt+z").await;
        tab.leave_compare();
        println!(
            "bench surface compare numbers={numbers} after={} unwrapped={unwrapped:?} \
             wrapped_again={}",
            tab.line_numbers().0,
            side.wrap_mode() != gtk::WrapMode::None
        );

        let Some(tab) = opened_tab(&app, &note).await else {
            return bench_quit(&app);
        };
        probe("tab_note", &tab.view).await;
        let staged = app.open_diff(
            &format!("staged:{note}"),
            &note,
            "Staged",
            ("Index", &other(&note_text)),
            ("Working Tree", &note_text),
            false,
        );
        settle().await;
        let [old, new] = staged.views().clone();
        probe("diff_old", &old).await;
        probe("diff_new", &new).await;
        press(&new, "alt+z").await;
        let unwrapped = [&old, &new].map(|v| v.wrap_mode() == gtk::WrapMode::None);
        press(&new, "alt+z").await;
        println!(
            "bench surface staged unwrapped={unwrapped:?} tab_width={}",
            new.tab_width()
        );

        app.open_path(&ipynb);
        settle().await;
        let Some(book) = app.active_notebook() else {
            println!("bench surface no_notebook");
            return bench_quit(&app);
        };
        for (n, name) in [(0, "cell_markdown"), (1, "cell_code")] {
            book.bench_select(n);
            settle().await;
            if let Some(view) = book.bench_view(n) {
                probe(name, &view).await;
            }
        }
        // The Indent Width preference, 4 unless set, reaches the cells as it reaches a tab.
        book.set_indent_width(2);
        let widths = [0, 1].map(|n| book.bench_view(n).map_or(0, |v| v.tab_width()));
        println!("bench surface cells tab_widths={widths:?}");
        bench_quit(&app);
    });
}

/// A short line, `wide` and a screen of lines under them, so a Down that scrolls can.
fn fixture(wide: &str) -> String {
    let tail: Vec<String> = (0..120).map(|i| format!("line {i}")).collect();
    format!("short\n{wide}\nshort\n{}\n", tail.join("\n"))
}

/// A line of code that wraps over several rows.
fn code_line() -> String {
    format!("let x = [{}];", "item, ".repeat(60))
}

/// The other side of a comparison: every line but the wrapped one changed, so no unchanged run
/// is hidden between them.
fn other(text: &str) -> String {
    text.lines()
        .enumerate()
        .map(|(i, line)| match i {
            1 => format!("{line}\n"),
            _ => format!("{line}.\n"),
        })
        .collect()
}

/// A notebook of a markdown cell and a code cell, each a short line, `wide` or `code`, and a short
/// line.
fn notebook_json(wide: &str, code: &str) -> String {
    let source = |line: &str| serde_json::json!(["short\n", format!("{line}\n"), "short"]);
    serde_json::json!({
        "cells": [
            {"cell_type": "markdown", "metadata": {}, "source": source(wide)},
            {"cell_type": "code", "execution_count": null, "metadata": {}, "outputs": [],
             "source": source(code)},
        ],
        "metadata": {"language_info": {"name": "python"}},
        "nbformat": 4,
        "nbformat_minor": 5,
    })
    .to_string()
}

/// `rel` opened and laid out, as the text tab in front.
async fn opened_tab(app: &Rc<App>, rel: &str) -> Option<Rc<Tab>> {
    app.open_path(rel);
    for _ in 0..100 {
        glib::timeout_future(Duration::from_millis(20)).await;
        if let Some(tab) = app.active().filter(|tab| tab.rel() == rel) {
            settle().await;
            return Some(tab);
        }
    }
    println!("bench surface no_tab {rel}");
    None
}

/// Time for a frame and the lines' layout.
async fn settle() {
    glib::timeout_future(Duration::from_millis(400)).await;
}

/// The keys of the drill on `view`, from the top of its text.
async fn probe(name: &str, view: &sourceview5::View) {
    view.grab_focus();
    let adjustment = view.vadjustment();
    if let Some(adjustment) = &adjustment {
        adjustment.set_value(0.0);
    }
    settle().await;
    let scroll = || adjustment.as_ref().map_or(0.0, |a| a.value());

    place(view, 1, 3);
    let before = scroll();
    press(view, "Down").await;
    let down = spot(view, &caret(view));
    let scrolled = scroll() != before;

    place(view, 1, 3);
    press(view, "End").await;
    let end = spot(view, &caret(view));

    place(view, 0, 3);
    press(view, "shift+alt+Down").await;
    let added = carets(view);
    press(view, "Down").await;
    let column = carets(view);
    if let Some(view) = view.downcast_ref::<multicaret::View>() {
        view.clear_carets();
    }

    place(view, 2, 1);
    view.clipboard().set_text("-");
    press(view, "ctrl+c").await;
    let copy = view.clipboard().read_text_future().await.ok().flatten();
    println!(
        "bench surface {name} down={down} end={end} col=[{}]->[{}] scrolled={scrolled} copy={:?}",
        added.join(" "),
        column.join(" "),
        copy.as_deref().unwrap_or("")
    );
}

/// The caret on `line` at `column`, nothing selected.
fn place(view: &sourceview5::View, line: i32, column: i32) {
    let buffer = view.buffer();
    if let Some(at) = buffer.iter_at_line_offset(line, column) {
        buffer.place_cursor(&at);
    }
}

fn caret(view: &sourceview5::View) -> gtk::TextIter {
    let buffer = view.buffer();
    buffer.iter_at_mark(&buffer.get_insert())
}

/// `<line>:<offset>:<row>`, the row counted from the line's first.
fn spot(view: &sourceview5::View, at: &gtk::TextIter) -> String {
    let mut row = *at;
    row.set_line_offset(0);
    let mut rows = 0;
    while view.forward_display_line(&mut row) && row.line() == at.line() && row <= *at {
        rows += 1;
    }
    format!("{}:{}:{rows}", at.line(), at.line_offset())
}

/// Every caret's [`spot`], the primary's first.
fn carets(view: &sourceview5::View) -> Vec<String> {
    let buffer = view.buffer();
    match view.downcast_ref::<multicaret::View>() {
        Some(column) => column
            .caret_positions()
            .into_iter()
            .filter_map(|(line, offset)| buffer.iter_at_line_offset(line, offset))
            .map(|at| spot(view, &at))
            .collect(),
        None => vec![spot(view, &caret(view))],
    }
}

/// `chord` pressed for real, through XTEST, with the keyboard in `view`.
async fn press(view: &sourceview5::View, chord: &str) {
    view.grab_focus();
    super::git::xtest(&format!("key {chord}")).await;
}
