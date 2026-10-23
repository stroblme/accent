//! Exercise the real bibliography surfaces and save path in a scratch vault.
use super::*;

pub fn run(app: &Rc<App>) {
    assert!(
        app.root().starts_with("/tmp"),
        "bibliography drill needs a scratch vault"
    );
    let app = app.clone();
    glib::spawn_future_local(async move {
        for _ in 0..600 {
            if app.reconciled.get() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        let vault = app.vault().unwrap().clone();
        vault.create_dir("library", true).unwrap();
        vault.create_dir("project", true).unwrap();
        let original = "% Keep my layout and comments.\n@article{smith_quantum_2024,\n  title = {A {Quantum} Paper},\n  author = {Smith, Jane},\n  year = {2024},\n  journal = {Physical Review},\n  doi = {10.1234/paper},\n  file = {:paper.pdf:PDF},\n}\n@book{other,title={Another Work},author={Jones, Sam},year=2020,publisher={Example}}\n";
        vault
            .write_file("library/main.bib", original.as_bytes())
            .unwrap();
        vault
            .write_file(
                "library/paper.pdf",
                b"%PDF-1.4\nfixture used only for attachment copying\n",
            )
            .unwrap();
        vault
            .write_file(
                "project/references.bib",
                b"% Externally maintained bibliography\n",
            )
            .unwrap();
        app.show_pane("files");
        app.open_path("library/main.bib");
        let tab = wait_tab(&app, "library/main.bib").await;
        assert!(
            app.sidebar.get().unwrap().is_showing("files"),
            "opening a bibliography must not switch sidebar panes"
        );
        let editor = tab.bibliography.borrow().clone().unwrap();
        assert!(editor.key_target().is_some());
        let bib = Library::parse(&tab.text()).unwrap();
        assert!(
            operations::transfer(
                &app,
                &tab,
                &bib,
                &[0],
                "project/../library/main.bib",
                true,
                false
            )
            .await
            .is_err()
        );
        assert_eq!(tab.text(), original);
        let after = bib.set_field(0, "year", "2025").unwrap();
        edit::apply(&tab, original, &after).unwrap();
        editor.entries.view.grab_focus();
        editor
            .entries
            .view
            .activate_action("win.bib-undo", None)
            .unwrap();
        assert_eq!(tab.text(), original);
        editor.source.set_active(true);
        assert!(editor.key_target().is_none());
        editor.source.set_active(false);
        operations::transfer(
            &app,
            &tab,
            &bib,
            &[0],
            "project/references.bib",
            false,
            false,
        )
        .await
        .unwrap();
        let project = vault.read("project/references.bib").unwrap().0;
        assert!(project.starts_with("% Externally maintained bibliography"));
        assert!(project.contains("../library/paper.pdf"));
        assert!(!project.contains("@book{other"));
        assert_eq!(tab.text(), original);
        operations::transfer(&app, &tab, &bib, &[0], "project/copied.bib", false, true)
            .await
            .unwrap();
        assert!(
            vault
                .read("project/copied.bib")
                .unwrap()
                .0
                .contains(":papers/paper.pdf:PDF")
        );
        assert_eq!(
            vault.read("project/papers/paper.pdf").unwrap().0,
            vault.read("library/paper.pdf").unwrap().0
        );
        operations::transfer(
            &app,
            &tab,
            &bib,
            &[1],
            "project/references.bib",
            true,
            false,
        )
        .await
        .unwrap();
        assert!(!tab.text().contains("@book{other"));
        assert!(
            vault
                .read("project/references.bib")
                .unwrap()
                .0
                .contains("@book{other")
        );
        let before = tab.text();
        tab.buffer.insert_at_cursor("% external edit\n");
        assert!(edit::apply(&tab, &before, &before).is_err());
        tab.buffer.undo();
        app.show_pane("bibliography");
        editor.entries.select("smith_quantum_2024");
        glib::timeout_future(Duration::from_millis(400)).await;
        app.toasts.dismiss_all();
        for (name, scheme) in [("light", "default"), ("dark", "prefer-dark")] {
            gio::Settings::new("org.gnome.desktop.interface")
                .set_string("color-scheme", scheme)
                .unwrap();
            glib::timeout_future(Duration::from_millis(300)).await;
            assert_eq!(adw::StyleManager::default().is_dark(), name == "dark");
            shot(
                &app.window.clone().upcast(),
                &format!("/tmp/accent-bibliography-{name}.png"),
            );
        }
        editor.edit(0);
        glib::timeout_future(Duration::from_millis(300)).await;
        shot(
            &app.window.clone().upcast(),
            "/tmp/accent-bibliography-edit.png",
        );
        if let Some(dialog) = app.window.visible_dialog() {
            dialog.close();
        }
        app.window.set_default_size(650, 700);
        glib::timeout_future(Duration::from_millis(400)).await;
        shot(
            &app.window.clone().upcast(),
            "/tmp/accent-bibliography-narrow.png",
        );
        println!(
            "bench bibliography source_preserved=true undo=true sidebar_unchanged=true copy=true move=true stale_review_refused=true"
        );
        app.window.application().unwrap().quit();
    });
}

async fn wait_tab(app: &App, path: &str) -> Rc<Tab> {
    for _ in 0..300 {
        if let Some(tab) = app.tab_for(path) {
            return tab;
        }
        glib::timeout_future(Duration::from_millis(50)).await;
    }
    panic!("bibliography did not open")
}
fn shot(widget: &gtk::Widget, path: &str) {
    let paintable = gtk::WidgetPaintable::new(Some(widget));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(
        &snapshot,
        f64::from(widget.width()),
        f64::from(widget.height()),
    );
    if let Some(node) = snapshot.to_node()
        && let Some(renderer) = widget.native().and_then(|n| n.renderer())
    {
        let texture = renderer.render_texture(&node, None);
        texture.save_to_png(path).unwrap();
    }
}
