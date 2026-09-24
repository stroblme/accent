//! The Replace All drill: what the Search pane lists once the vault has been rewritten under it.

use super::*;

/// A word nothing in a vault would already hold, so the rewrite reaches one note and one note
/// only — and, being one match, applies without the confirmation Xvfb cannot answer.
const MARKER: &str = "zzreplacemarker";
/// The note the drill writes and takes away again, so it never rewrites anything of the reader's.
const NOTE: &str = "accent-bench-replace.md";
/// Long enough for the index to take the note in, and for the query's 50 ms debounce plus the
/// worker thread behind it.
const SETTLE: Duration = Duration::from_millis(1200);

/// `ACCENT_BENCH_REPLACE=1` writes a note holding one unique word, searches for it with the
/// replace row open, presses Replace All and prints what the pane lists before and after.
///
/// `before` must be `page=results rows=1` and `after` `page=empty rows=0`: the rows standing
/// after the rewrite are the ones the new text answers for.
///
/// One note is all a drill can rewrite — two would raise the confirmation Xvfb cannot answer —
/// and one note is also the case where the worker has indexed the rewrite before the requery
/// reaches the main loop anyway. So this covers the chain, not the race the requery used to
/// lose: that one is `replace_all_rewrites_every_match_and_reindexes` in accent-api, which
/// greps the instant the call returns.
pub(super) fn bench_replace(app: &Rc<App>) {
    let path = app.root().join(NOTE);
    if let Err(e) = std::fs::write(&path, format!("a line holding {MARKER} once\n")) {
        println!("bench replace wrote=false {e}");
        return bench_quit(app);
    }
    let app = app.clone();
    glib::timeout_add_local_once(SETTLE, move || {
        let Some(sidebar) = app.sidebar.get() else {
            return bench_replace_done(&app, &path);
        };
        sidebar.show_replace();
        sidebar.set_search_text(MARKER);
        sidebar.set_replace_text("zzreplaced");
        glib::timeout_add_local_once(SETTLE, move || {
            let Some(sidebar) = app.sidebar.get() else {
                return bench_replace_done(&app, &path);
            };
            bench_replace_print(sidebar, "before");
            sidebar.press_replace_all();
            glib::timeout_add_local_once(SETTLE, move || {
                if let Some(sidebar) = app.sidebar.get() {
                    bench_replace_print(sidebar, "after");
                }
                bench_replace_done(&app, &path);
            });
        });
    });
}

fn bench_replace_print(sidebar: &crate::sidebar::Sidebar, step: &str) {
    let (page, rows, count) = sidebar.search_state();
    println!("bench replace step={step} page={page} rows={rows} count={count:?}");
}

/// Take the drill's own note away again, whatever it managed to do with it.
fn bench_replace_done(app: &Rc<App>, path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    bench_quit(app);
}
