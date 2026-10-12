//! The Replace All drill: what the Search pane lists once the vault has been rewritten under it.

use super::files::until;
use super::*;

/// Unique fixture word restricting replacement to one note, avoiding the multi-file
/// confirmation that Xvfb cannot answer.
const MARKER: &str = "zzreplacemarker";
/// The note the drill writes and takes away again, so it never rewrites anything of the reader's.
const NOTE: &str = "accent-bench-replace.md";
/// Long enough for a rewrite or an undo, and the query the pane asks again behind it.
const SETTLE: Duration = Duration::from_millis(1200);

/// `ACCENT_BENCH_REPLACE=1` creates a note with a unique word and invokes Replace All.
/// Require `before page=results rows=1`, `after page=empty rows=0`, and Undo
/// `undone page=results rows=1` with the word restored. Replace again, modify the
/// file externally, then Undo: `undo_skipped` must preserve the external edit.
/// `replaced_again` must contain one "Replaced" toast so its Undo targets the latest edit.
///
/// This single-note drill avoids the confirmation Xvfb cannot answer. It checks the
/// UI chain; `replace_all_rewrites_every_match_and_reindexes` in accent-api checks
/// index consistency immediately on return.
pub(super) fn bench_replace(app: &Rc<App>) {
    let path = app.root().join(NOTE);
    if let Err(e) = std::fs::write(&path, format!("a line holding {MARKER} once\n")) {
        println!("bench replace wrote=false {e}");
        return bench_quit(app);
    }
    let app = app.clone();
    glib::spawn_future_local(async move {
        // The index answers the search, and a cold one takes the note in only once its first walk
        // is over: many seconds on `make vault`.
        until(|| app.reconciled.get()).await;
        let Some(sidebar) = app.sidebar.get() else {
            return bench_replace_done(&app, &path);
        };
        sidebar.show_replace();
        sidebar.set_search_text(MARKER);
        sidebar.set_replace_text("zzreplaced");
        // Until the note's row is listed: a query asked before the worker took the note in gets it
        // from the requery the note's update brings.
        until(|| sidebar.search_state().0 == "results").await;
        bench_replace_print(&app, &path, "before");
        sidebar.press_replace_all();
        glib::timeout_add_local_once(SETTLE, move || {
            bench_replace_print(&app, &path, "after");
            app.undo_replace();
            glib::timeout_add_local_once(SETTLE, move || {
                bench_replace_print(&app, &path, "undone");
                bench_replace_skip(app, path);
            });
        });
    });
}

/// Replace again, change the note before undoing, and print what the undo left in it.
fn bench_replace_skip(app: Rc<App>, path: std::path::PathBuf) {
    let Some(sidebar) = app.sidebar.get() else {
        return bench_replace_done(&app, &path);
    };
    sidebar.press_replace_all();
    glib::timeout_add_local_once(SETTLE, move || {
        bench_replace_print(&app, &path, "replaced_again");
        let _ = std::fs::write(&path, "edited after the replace\n");
        app.undo_replace();
        glib::timeout_add_local_once(SETTLE, move || {
            bench_replace_print(&app, &path, "undo_skipped");
            bench_replace_done(&app, &path);
        });
    });
}

fn bench_replace_print(app: &Rc<App>, path: &std::path::Path, step: &str) {
    let Some(sidebar) = app.sidebar.get() else {
        return println!("bench replace step={step} pane=none");
    };
    let (page, rows, count) = sidebar.search_state();
    let rows = rows.len();
    let text = std::fs::read_to_string(path).unwrap_or_default();
    println!(
        "bench replace step={step} page={page} rows={rows} count={count:?} text={text:?} \
         toasts={:?}",
        app.toasts.shown()
    );
}

/// Take the drill's own note away again, whatever it managed to do with it.
fn bench_replace_done(app: &Rc<App>, path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    bench_quit(app);
}
