//! Verify ignored notes remain reachable through Go to File, completion and links.

use super::*;
use accent_api::Pos;

/// `ACCENT_BENCH_SWITCHER=ignored:<rel>` needs an ignored folder with `Deep Note.md`
/// and a note `<rel>` containing `[[Deep Note]] [[Nowhere]]` and `[[Deep` on line two.
/// After indexing, print ignored-folder walk time/count, Go to File rows, hints,
/// completion and the tab opened by following `[[Deep Note]]`. Use a scratch vault.
pub(super) fn bench_ignored(app: &Rc<App>, rel: &str) {
    let Some(vault) = app.vault().cloned() else {
        return bench_quit(app);
    };
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        for _ in 0..600 {
            if app.reconciled.get() {
                break;
            }
            glib::timeout_future(Duration::from_millis(200)).await;
        }
        // For the palette's list, which the reconcile asked for, to land: it is what the dialog
        // shows on opening.
        glib::timeout_future(Duration::from_millis(1000)).await;
        // That list walked the ignored folders once already: this times the walk Go to File
        // opening asks for.
        let walked = crate::work::off_thread("walk", move || {
            let t0 = Instant::now();
            let notes = vault.ignored_notes(true).map(|n| n.len());
            (ms_since(t0), notes)
        })
        .await;
        if let Some((ms, notes)) = walked {
            println!("bench ignored_walk ms={ms:.1} notes={notes:?}");
        }

        let _ = WidgetExt::activate_action(&app.window, "win.palette-files", None);
        let Some(dialog) = app.window.visible_dialog() else {
            return bench_quit(&app);
        };
        if let Some(entry) = find_search_entry(dialog.upcast_ref()) {
            entry.set_text("Deep Note");
        }
        glib::timeout_future(Duration::from_millis(1500)).await;
        bench_switcher_rows(&app);
        dialog.close();

        app.open_path(&rel);
        let mut hints = Vec::new();
        for _ in 0..50 {
            glib::timeout_future(Duration::from_millis(100)).await;
            if let Some(tab) = app.open_tabs().into_iter().find(|t| t.rel() == rel) {
                hints = tab
                    .diagnostics()
                    .iter()
                    .map(|d| d.message.clone())
                    .collect();
            }
            if !hints.is_empty() {
                break;
            }
        }
        println!("bench ignored_hints {hints:?}");

        let Some(vault) = app.vault().cloned() else {
            return bench_quit(&app);
        };
        let at = Pos {
            line: 1,
            character: 6,
        };
        let rows: Vec<(String, Option<String>)> = match vault.completion(&rel, at, None).await {
            Ok(answer) => answer
                .items
                .into_iter()
                .map(|i| (i.insert, i.detail))
                .collect(),
            Err(e) => vec![(format!("error: {e}"), None)],
        };
        println!("bench ignored_completion {rows:?}");

        app.open_target("Deep Note");
        glib::timeout_future(Duration::from_millis(1000)).await;
        println!("bench ignored_followed {:?}", app.active_key());
        bench_quit(&app);
    });
}
