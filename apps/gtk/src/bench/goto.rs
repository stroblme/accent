//! Go to File: where the keyboard is once the picked note is in front.

use super::*;

/// `ACCENT_BENCH_SWITCHER=goto:<relA>,<relB>` picks notes, each named by a stem of letters,
/// digits and spaces, in Go to File with real XTEST keys (`Ctrl+E`, the stem, Return): `<relA>`
/// from the Files tree with no tab open, as on a fresh start, `<relB>` from `<relA>`'s text, and
/// `<relA>`, open behind it, from the tree again. Prints `bench goto_focus <case> <in_note>
/// <focused widget>` once each is in front and exits 1 where the note has not the keyboard.
pub(super) fn bench_goto(app: &Rc<App>, rels: &str) {
    let Some((a, b)) = rels.split_once(',') else {
        return bench_quit(app);
    };
    let (app, a, b) = (app.clone(), a.to_string(), b.to_string());
    glib::spawn_future_local(async move {
        for _ in 0..600 {
            if app.reconciled.get() {
                break;
            }
            glib::timeout_future(Duration::from_millis(200)).await;
        }
        // For the palette's list, which the reconcile asked for, to land.
        glib::timeout_future(Duration::from_millis(1000)).await;
        // No window manager gives the window the keyboard.
        super::git::xtest("move 400 300; focus").await;
        let mut taken = true;
        for (case, rel) in [("tree", &a), ("editor", &b), ("tree_open", &a)] {
            let origin: Option<gtk::Widget> = match case {
                "editor" => app.tab_for(&a).map(|tab| tab.view.clone().upcast()),
                _ => app.tree.get().map(|tree| tree.view().clone().upcast()),
            };
            let Some(origin) = origin else {
                println!("bench goto_focus {case} no_origin");
                std::process::exit(1);
            };
            origin.grab_focus();
            let stem = Path::new(rel.as_str())
                .file_stem()
                .map_or(String::new(), |s| s.to_string_lossy().to_string());
            super::git::xtest(&format!(
                "key ctrl+e; sleep 0.5; type {stem}; sleep 0.8; key Return"
            ))
            .await;
            for _ in 0..50 {
                if app.active_key().as_deref() == Some(rel.as_str()) {
                    break;
                }
                glib::timeout_future(Duration::from_millis(100)).await;
            }
            // The palette's closing, and the idles that follow a landing.
            glib::timeout_future(Duration::from_millis(800)).await;
            let focus = app.focused();
            let page = app.doc_for(rel).map(|doc| doc.page().child());
            let in_note = focus
                .as_ref()
                .zip(page.as_ref())
                .is_some_and(|(focus, page)| focus == page || focus.is_ancestor(page));
            let name = focus.as_ref().map(|w| w.type_().name());
            println!("bench goto_focus {case} {in_note} {name:?}");
            taken &= in_note;
        }
        if !taken {
            std::process::exit(1);
        }
        bench_quit(&app);
    });
}
