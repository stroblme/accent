//! Check Tags updates as note tags change and preview clicks select them.

use super::*;

/// A tag no vault would already carry, so its presence in the list is the whole answer.
const MARKER: &str = "zzbenchtag";
/// Long enough for the save to land, the worker to index the note, the pane's 300 ms settle and
/// the query it then runs. A local vault answers all four in a few milliseconds each.
const SETTLE: Duration = Duration::from_millis(1200);

/// `ACCENT_BENCH_TAGS=<rel_note>` adds, saves, clicks and removes `#zzbenchtag`.
/// Require the marker absent in `tags_before/removed`, present in `tags_added`, and
/// selected in `tags_clicked`, without reopening the pane. Restore the original note.
pub(super) fn bench_tags(app: &Rc<App>, rel: &str) {
    app.show_section("tags");
    app.open_path(rel);
    let app = app.clone();
    let rel = rel.to_string();
    glib::timeout_add_local_once(SETTLE, move || {
        let Some(tab) = app.tab_for(&rel) else {
            return bench_quit(&app);
        };
        bench_tags_print(&app, "before");
        // A tag picked before the edit, so the step after it says whether the refill left the
        // reader where they were: a list that snaps shut under the pointer is no better than one
        // that never moves.
        if let (Some(sidebar), Some(first)) = (app.sidebar.get(), first_tag(&app)) {
            sidebar.show_tag(&first);
        }
        // Inserted rather than `set_text`, which loads a buffer instead of editing one: only an
        // edit marks the tab modified, and only a modified tab is written.
        let original = tab.text();
        tab.buffer
            .insert(&mut tab.buffer.end_iter(), &format!("\n#{MARKER}\n"));
        app.save_tab(&tab, true);
        glib::timeout_add_local_once(SETTLE, move || {
            bench_tags_print(&app, "added");
            glib::spawn_future_local(async move {
                click_marker(&app).await;
                bench_tags_print(&app, "clicked");
                tab.set_text(&original);
                tab.save.modified.set(true);
                app.save_tab(&tab, true);
                glib::timeout_add_local_once(SETTLE, move || {
                    bench_tags_print(&app, "removed");
                    bench_quit(&app);
                });
            });
        });
    });
}

/// Show the note's preview beside it and follow the marker's tag link there, as a click on it
/// does.
async fn click_marker(app: &Rc<App>) {
    super::preview::beside(app).await;
    glib::timeout_future(SETTLE).await;
    let found = super::preview::shown(app).and_then(|r| {
        let preview = r.preview();
        preview.as_ref().map(|p| p.follow_tag(MARKER))
    });
    match found {
        Some(found) => println!("bench tags link={found}"),
        None => return println!("bench tags step=clicked preview=none"),
    }
    glib::timeout_future(Duration::from_millis(300)).await;
}

/// What the section lists at one step, with the marker called out so a long list still answers
/// the question at a glance.
fn bench_tags_print(app: &Rc<App>, step: &str) {
    let Some(sidebar) = app.sidebar.get() else {
        return println!("bench tags step={step} pane=none");
    };
    let names = sidebar.tag_names();
    println!(
        "bench tags step={step} showing={} marker={} rows={} picked={:?}",
        sidebar.section_live("tags"),
        names.iter().any(|name| name == MARKER),
        names.len(),
        sidebar.selected_tag()
    );
}

/// The first tag the section lists, whatever the vault happens to hold.
fn first_tag(app: &Rc<App>) -> Option<String> {
    app.sidebar.get()?.tag_names().into_iter().next()
}
