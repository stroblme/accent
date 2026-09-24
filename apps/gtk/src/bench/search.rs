//! The Search pane drill: whether its rows follow the vault under a query that is already up.

use super::*;

/// The files the drill writes and takes away again, so it rewrites nothing of the reader's.
const NOTE: &str = "accent-bench-search";
/// One watcher debounce (300 ms), the index batch behind it and the pane's own settle, with room
/// for a large vault's worker to get to it.
const SETTLE: Duration = Duration::from_millis(2500);
/// How long to wait for a cold vault's first walk before giving up on it. `testvault` is 40k
/// files and takes about half a minute.
const INDEXED: Duration = Duration::from_secs(120);

/// `ACCENT_BENCH_SEARCH=<query>[:<n>]` puts `<query>` in the Search pane, then writes `n` files
/// holding it twice — one note unless the tail says otherwise, every second file a `.txt` rather
/// than a note — and takes them away again, printing what the pane lists at each step and what
/// the Replace All button says. The whole cycle runs twice: once under ranked full text, and once
/// with the replace row open, which is the exact scan Replace All needs.
///
/// `before` and `removed` must match, and `added` must have `2n` rows more: a row is a match and
/// not a file, in either mode. That is the pane following the vault without the box being
/// touched. Pass a word the vault does not already hold and the steps read `rows=0`, `rows=2n`,
/// `rows=0` in both modes.
///
/// The button counts only under the exact scan, where it must say what the rewrite would touch:
/// with such a word, `Replace All (0)` at `grep`, `grep_removed` and `away_removed`, and
/// `Replace All (2n)` at `grep_added` and `away_added`, the `.txt` files included — Replace All
/// rewrites every indexed file, not only the notes. The ranked steps read `Replace All (0)`, not
/// pressable.
///
/// The last two steps stage the same batch with the *Files* pane in front instead, so
/// `away_added` and `away_removed` are the catch-up a pane that was not on screen owes when it
/// comes back: `rows=2n` and `rows=0` again, not the rows it was left with.
///
/// `RUST_LOG=accent=debug` prints `sidebar query … grep=… ms=…` for every query the run makes, so
/// the `grep=true` lines after the `grep` step are what a requery nobody asked for costs in exact
/// mode, and `<n>` is how many files the settled batch behind one of them touched.
pub(super) fn bench_search(app: &Rc<App>, arg: &str) {
    // A `:<n>` tail stages a batch of several files, which is what a sync pull looks like; a
    // query with no such tail is the one note an autosave writes.
    let (query, count) = match arg
        .rsplit_once(':')
        .and_then(|(q, n)| Some((q, n.parse::<usize>().ok()?)))
    {
        Some((query, count)) => (query.to_string(), count.max(1)),
        None => (arg.to_string(), 1),
    };
    bench_search_indexed(app.clone(), query, count, Instant::now());
}

/// Nothing can be searched for until the first walk is over, and on a large vault that is not
/// the 400 ms every other drill starts after.
fn bench_search_indexed(app: Rc<App>, query: String, count: usize, since: Instant) {
    if !app.reconciled.get() {
        if since.elapsed() > INDEXED {
            println!("bench search indexed=false");
            return bench_quit(&app);
        }
        glib::timeout_add_local_once(Duration::from_millis(200), move || {
            bench_search_indexed(app, query, count, since)
        });
        return;
    }
    let Some(sidebar) = app.sidebar.get() else {
        return bench_quit(&app);
    };
    sidebar.show_pane("search");
    sidebar.set_search_text(&query);
    glib::timeout_add_local_once(SETTLE, move || {
        bench_search_print(&app, "before");
        bench_search_cycle(app, query, count, "", |app, query, count| {
            // The exact scan, for the half of the cost that is not an FTS lookup — and then the
            // same batch again under it, which is the requery the reader did not ask for.
            if let Some(sidebar) = app.sidebar.get() {
                sidebar.show_replace();
            }
            glib::timeout_add_local_once(SETTLE, move || {
                bench_search_print(&app, "grep");
                bench_search_cycle(app, query, count, "grep_", |app, query, count| {
                    // And last, the same batch again with another pane in front of this one.
                    let paths = bench_search_paths(&app, count);
                    let gone = paths.clone();
                    bench_search_away(
                        app,
                        "away_added",
                        move || bench_search_write(&paths, &query),
                        move |app| {
                            bench_search_away(
                                app,
                                "away_removed",
                                move || bench_search_remove(&gone),
                                |app| bench_quit(&app),
                            )
                        },
                    );
                });
            });
        });
    });
}

/// Write the marker files behind the pane's back — which is what an edit in another editor is, the
/// vault's own watcher being what has to carry it — then take them away again, printing the rows
/// after each. `mode` names which mode the pane was in, and `then` is what the run does next.
fn bench_search_cycle(
    app: Rc<App>,
    query: String,
    count: usize,
    mode: &'static str,
    then: impl FnOnce(Rc<App>, String, usize) + 'static,
) {
    let paths = bench_search_paths(&app, count);
    bench_search_write(&paths, &query);
    glib::timeout_add_local_once(SETTLE, move || {
        bench_search_print(&app, &format!("{mode}added"));
        bench_search_remove(&paths);
        glib::timeout_add_local_once(SETTLE, move || {
            bench_search_print(&app, &format!("{mode}removed"));
            then(app, query, count);
        });
    });
}

/// One half of the catch-up: leave the Search pane for the Files pane, move the vault under it
/// with `act`, come back, and print what the rows say once it has answered for the change it
/// never saw. Without the pane's dirty flag these are the rows it had when it was left.
fn bench_search_away(
    app: Rc<App>,
    step: &'static str,
    act: impl FnOnce() + 'static,
    then: impl FnOnce(Rc<App>) + 'static,
) {
    let Some(sidebar) = app.sidebar.get() else {
        return bench_quit(&app);
    };
    sidebar.show_pane("files");
    act();
    glib::timeout_add_local_once(SETTLE, move || {
        if let Some(sidebar) = app.sidebar.get() {
            sidebar.show_pane("search");
        }
        // The query the switch starts runs on a worker thread like any other, so the rows are
        // read a moment later rather than in the same turn of the main loop.
        glib::timeout_add_local_once(SETTLE, move || {
            bench_search_print(&app, step);
            then(app);
        });
    });
}

/// Where the marker files go: one per file of the batch the drill stages, every second one a
/// `.txt`, which the exact scan and Replace All reach just as they reach a note.
fn bench_search_paths(app: &Rc<App>, count: usize) -> Vec<PathBuf> {
    (0..count)
        .map(|i| {
            let ext = if i % 2 == 1 { "txt" } else { "md" };
            app.root().join(format!("{NOTE}-{i}.{ext}"))
        })
        .collect()
}

fn bench_search_write(paths: &[PathBuf], query: &str) {
    for path in paths {
        // Twice, on two lines: a row is a match and not a file, so each file written here is two
        // rows in both modes.
        let body = format!("a line holding {query} once\nand a second line holding {query}\n");
        if let Err(e) = std::fs::write(path, body) {
            println!("bench search wrote=false {e}");
        }
    }
}

fn bench_search_remove(paths: &[PathBuf]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

fn bench_search_print(app: &Rc<App>, step: &str) {
    let Some(sidebar) = app.sidebar.get() else {
        return println!("bench search step={step} pane=none");
    };
    let (page, rows, count) = sidebar.search_state();
    let (button, sensitive) = sidebar.replace_all_state();
    println!(
        "bench search step={step} page={page} rows={rows} count={count:?} button=\"{button}\" \
         sensitive={sensitive}"
    );
}
