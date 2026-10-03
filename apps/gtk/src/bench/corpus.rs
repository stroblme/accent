//! Go to File's list as it lands: in a dialog opened before the vault was indexed, and fresh on
//! the opening after a note was saved.

use super::*;

/// `ACCENT_BENCH_SWITCHER=early:<query>` opens Go to File at once, types `<query>` and prints its
/// first rows (`early_rows open`); then, the dialog still up, once the vault is indexed and again
/// (`early_rows indexed`). Then it saves `Fresh Links.md`, linking to a note not written yet and
/// carrying an alias, closes the dialog, opens it again and prints the rows for the link's target
/// and for the alias (`early_rows saved`): both are there on that opening, not the one after. It
/// writes a note, so point it at a scratch vault under `/tmp`.
pub(super) fn bench_early(app: &Rc<App>, query: &str) {
    scratch_only(app, "ACCENT_BENCH_SWITCHER=early");
    let (app, query) = (app.clone(), query.to_string());
    glib::spawn_future_local(async move {
        let typed = |text: &str| {
            let entry = app
                .window
                .visible_dialog()
                .and_then(|d| find_search_entry(d.upcast_ref()));
            if let Some(entry) = entry {
                entry.set_text(text);
            }
        };
        let rows = |when: &str| {
            println!(
                "bench early_rows {when} reconciled={}",
                app.reconciled.get()
            );
            bench_switcher_rows(&app);
        };
        let _ = WidgetExt::activate_action(&app.window, "win.palette-files", None);
        typed(&query);
        glib::timeout_future(Duration::from_millis(1500)).await;
        rows("open");
        for _ in 0..600 {
            if app.reconciled.get() {
                break;
            }
            glib::timeout_future(Duration::from_millis(200)).await;
        }
        glib::timeout_future(Duration::from_secs(2)).await;
        rows("indexed");

        let Some(vault) = app.vault().cloned() else {
            return bench_quit(&app);
        };
        let note = "---\naliases: [Fresh Alias]\n---\nSee [[Brand New Target]].\n";
        if let Err(e) = vault.save("Fresh Links.md", note, None) {
            println!("bench early_save_failed {e:?}");
        }
        glib::timeout_future(Duration::from_secs(1)).await;
        if let Some(dialog) = app.window.visible_dialog() {
            dialog.close();
        }
        glib::timeout_future(Duration::from_millis(500)).await;
        let _ = WidgetExt::activate_action(&app.window, "win.palette-files", None);
        for text in ["Brand New Target", "Fresh Alias"] {
            typed(text);
            glib::timeout_future(Duration::from_millis(1500)).await;
            rows("saved");
        }
        bench_quit(&app);
    });
}
