//! The Search pane drill: whether its rows follow the vault under a query that is already up.

use super::*;

/// The note the drill writes and takes away again, so it rewrites nothing of the reader's.
const NOTE: &str = "accent-bench-search.md";
/// One watcher debounce (300 ms), the index batch behind it and the pane's own settle, with room
/// for a large vault's worker to get to it.
const SETTLE: Duration = Duration::from_millis(2500);
/// How long to wait for a cold vault's first walk before giving up on it. `testvault` is 40k
/// files and takes about half a minute.
const INDEXED: Duration = Duration::from_secs(120);

/// `ACCENT_BENCH_SEARCH=<query>` puts `<query>` in the Search pane, then writes a note holding it
/// and takes the note away again, printing what the pane lists at each step.
///
/// `before` and `removed` must match, and `added` must have one row more: that is the pane
/// following the vault without the box being touched. Pass a word the vault does not already
/// hold and the three read `rows=0`, `rows=1`, `rows=0`.
///
/// The last step opens the replace row, which switches the pane from ranked full text to the
/// exact scan Replace All needs, so one run times both modes: `RUST_LOG=accent=debug` prints
/// `sidebar query … grep=… ms=…` for each.
pub(super) fn bench_search(app: &Rc<App>, query: &str) {
    let (app, query) = (app.clone(), query.to_string());
    bench_search_indexed(app, query, Instant::now());
}

/// Nothing can be searched for until the first walk is over, and on a large vault that is not
/// the 400 ms every other drill starts after.
fn bench_search_indexed(app: Rc<App>, query: String, since: Instant) {
    if !app.reconciled.get() {
        if since.elapsed() > INDEXED {
            println!("bench search indexed=false");
            return bench_quit(&app);
        }
        glib::timeout_add_local_once(Duration::from_millis(200), move || {
            bench_search_indexed(app, query, since)
        });
        return;
    }
    let path = app.root().join(NOTE);
    let Some(sidebar) = app.sidebar.get() else {
        return bench_quit(&app);
    };
    sidebar.show_pane("search");
    sidebar.set_search_text(&query);
    glib::timeout_add_local_once(SETTLE, move || {
        bench_search_print(&app, "before");
        // Written behind the pane's back, which is what an edit in another editor is. The vault's
        // own watcher is what has to carry it, so nothing here tells the window about it.
        if let Err(e) = std::fs::write(&path, format!("a line holding {query} once\n")) {
            println!("bench search wrote=false {e}");
            return bench_quit(&app);
        }
        glib::timeout_add_local_once(SETTLE, move || {
            bench_search_print(&app, "added");
            let _ = std::fs::remove_file(&path);
            glib::timeout_add_local_once(SETTLE, move || {
                bench_search_print(&app, "removed");
                // The exact scan, for the half of the cost that is not an FTS lookup.
                if let Some(sidebar) = app.sidebar.get() {
                    sidebar.show_replace();
                }
                glib::timeout_add_local_once(SETTLE, move || {
                    bench_search_print(&app, "grep");
                    bench_quit(&app);
                });
            });
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
