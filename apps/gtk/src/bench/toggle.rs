//! Toggle Preview by its real chord: which tab each `Ctrl+M` brings to the front.

use super::*;

/// `ACCENT_BENCH_TABS=toggle:<note>,<other note>` presses `Ctrl+M` through XTEST from where a
/// reader presses it: `<note>` picked in Go to File, its preview clicked into, the Files tree,
/// after a switch to `<other note>` and back, twice in quick succession, with the preview in a
/// pane beside the note, after closing it, and on `<other note>` looked at from the tree. Prints
/// `bench toggle <case> <tab in front> <keyboard in it>` after each, and exits 1 where a press
/// left another tab in front or the keyboard outside it.
pub(super) fn bench_toggle(app: &Rc<App>, rels: &str) {
    let Some((a, b)) = rels.split_once(',') else {
        return bench_quit(app);
    };
    let (app, a, b) = (app.clone(), a.to_string(), b.to_string());
    glib::spawn_future_local(async move {
        until("toggle", "reconciled", 120_000, || app.reconciled.get()).await;
        glib::timeout_future(Duration::from_millis(1000)).await;
        // No window manager gives the window the keyboard.
        super::git::xtest("move 700 400; focus").await;
        let pick = |rel: &str| {
            let stem = Path::new(rel)
                .file_stem()
                .map_or(String::new(), |s| s.to_string_lossy().to_string());
            format!("key ctrl+e; sleep 0.5; type {stem}; sleep 0.8; key Return; sleep 0.8")
        };
        let rendered = || app.tab_for(&a).and_then(|tab| app.rendered_of(&tab.page));
        let (note, preview) = (a.as_str(), crate::rendered::key_of(&a));
        let mut right = true;
        let mut press = async |case: &str, keys: &str, want: &str| {
            super::git::xtest(keys).await;
            glib::timeout_future(Duration::from_millis(400)).await;
            let front = app.active_key().unwrap_or_default();
            let page = app.active_doc().map(|doc| doc.page().child());
            let held = app
                .focused()
                .zip(page)
                .is_some_and(|(focus, page)| focus == page || focus.is_ancestor(&page));
            println!("bench toggle {case} {front} {held}");
            right &= front == want && held;
        };
        super::git::xtest(&pick(&a)).await;
        press("picked", "key ctrl+m", &preview).await;
        press("picked", "key ctrl+m", note).await;
        press("again", "key ctrl+m", &preview).await;
        if let Some(rendered) = rendered() {
            rendered.key_target().grab_focus();
        }
        press("clicked", "key ctrl+m", note).await;
        if let Some(tree) = app.tree.get() {
            tree.view().grab_focus();
        }
        press("tree", "key ctrl+m", &preview).await;
        super::git::xtest(&format!("{}; {}", pick(&b), pick(&a))).await;
        press("switched", "key ctrl+m", &preview).await;
        press("rapid", "key ctrl+m; sleep 0.05; key ctrl+m", &preview).await;
        if let Some(page) = rendered().map(|r| r.page.clone())
            && let Some(pane) = app.pane_of(&page)
        {
            app.split_page(&pane, crate::panes::Side::Right, &page);
        }
        glib::timeout_future(Duration::from_millis(800)).await;
        press("beside", "key ctrl+m", note).await;
        press("beside", "key ctrl+m", &preview).await;
        if let Some(page) = rendered().map(|r| r.page.clone()) {
            app.close_page(&page);
        }
        glib::timeout_future(Duration::from_millis(800)).await;
        press("closed", "key ctrl+m", &preview).await;
        if let Some(tree) = app.tree.get() {
            tree.view().grab_focus();
        }
        app.open_preview(&b);
        glib::timeout_future(Duration::from_millis(1000)).await;
        press("looked", "key ctrl+m", &crate::rendered::key_of(&b)).await;
        press("looked", "key ctrl+m", &b).await;
        if !right {
            std::process::exit(1);
        }
        bench_quit(&app);
    });
}
