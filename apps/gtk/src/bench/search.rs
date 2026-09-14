//! The Search pane drill: whether its rows follow the vault under a query that is already up.

use super::*;

/// The notes the drill writes and takes away again, so it rewrites nothing of the reader's.
const NOTE: &str = "accent-bench-search";
/// One watcher debounce (300 ms), the index batch behind it and the pane's own settle, with room
/// for a large vault's worker to get to it.
const SETTLE: Duration = Duration::from_millis(2500);
/// How long to wait for a cold vault's first walk before giving up on it. `testvault` is 40k
/// files and takes about half a minute.
const INDEXED: Duration = Duration::from_secs(120);

/// `ACCENT_BENCH_SEARCH=<query>[:<n>]` puts `<query>` in the Search pane, then writes `n` notes
/// holding it — one unless the tail says otherwise — and takes them away again, printing what the
/// pane lists at each step. The whole cycle runs twice: once under ranked full text, and once with
/// the replace row open, which is the exact scan Replace All needs.
///
/// `before` and `removed` must match, and `added` must have `n` rows more: that is the pane
/// following the vault without the box being touched. Pass a word the vault does not already
/// hold and the steps read `rows=0`, `rows=n`, `rows=0` in both modes.
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
                bench_search_cycle(app, query, count, "grep_", |app, _, _| bench_quit(&app));
            });
        });
    });
}

/// Write the marker notes behind the pane's back — which is what an edit in another editor is, the
/// vault's own watcher being what has to carry it — then take them away again, printing the rows
/// after each. `mode` names which mode the pane was in, and `then` is what the run does next.
fn bench_search_cycle(
    app: Rc<App>,
    query: String,
    count: usize,
    mode: &'static str,
    then: impl FnOnce(Rc<App>, String, usize) + 'static,
) {
    let paths: Vec<PathBuf> = (0..count)
        .map(|i| app.root().join(format!("{NOTE}-{i}.md")))
        .collect();
    for path in &paths {
        if let Err(e) = std::fs::write(path, format!("a line holding {query} once\n")) {
            println!("bench search wrote=false {e}");
            return bench_quit(&app);
        }
    }
    glib::timeout_add_local_once(SETTLE, move || {
        bench_search_print(&app, &format!("{mode}added"));
        for path in &paths {
            let _ = std::fs::remove_file(path);
        }
        glib::timeout_add_local_once(SETTLE, move || {
            bench_search_print(&app, &format!("{mode}removed"));
            then(app, query, count);
        });
    });
}

fn bench_search_print(app: &Rc<App>, step: &str) {
    let Some(sidebar) = app.sidebar.get() else {
        return println!("bench search step={step} pane=none");
    };
    let (page, rows) = sidebar.search_state();
    println!("bench search step={step} page={page} rows={rows}");
}
