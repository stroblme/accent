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
        let original = "% Keep my layout and comments.\n@article{smith_quantum_2024,\n  title = {A {Quantum} Paper},\n  author = {Smith, Jane},\n  year = {2024},\n  journal = {Physical Review},\n  doi = {10.1234/paper},\n  file = {:paper.pdf:PDF},\n}\n@book{other,title={Another Work},author={Jones, Sam},year=2020,publisher={Example}}\n@article{lee_readout_2025,title={Fast high-fidelity readout of superconducting qubits},author={Lee, Morgan and Patel, Alex},year=2025,journal={Quantum},file={:paper.pdf:PDF}}\n@article{garcia_control_2023,title={Robust control in open quantum systems},author={García, Elena},year=2023,journal={Physical Review A}}\n";
        vault
            .write_file("library/a-long-bibliography-filename-that-must-fit-the-sidebar-without-widening-it-references.bib", original.as_bytes())
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
        app.open_path("library/a-long-bibliography-filename-that-must-fit-the-sidebar-without-widening-it-references.bib");
        let tab = wait_tab(&app, "library/a-long-bibliography-filename-that-must-fit-the-sidebar-without-widening-it-references.bib").await;
        assert!(
            app.sidebar.get().unwrap().is_showing("files"),
            "opening a bibliography must not switch sidebar panes"
        );
        glib::timeout_future(Duration::from_millis(100)).await;
        assert!(
            tab.view.is_drawable(),
            "bibliographies always open as source"
        );
        let panel = app.bibliography.get().unwrap();
        let bib = Library::parse(&tab.text()).unwrap();
        assert!(
            operations::transfer(
                &app,
                &tab,
                &bib,
                &[0],
                "project/../library/a-long-bibliography-filename-that-must-fit-the-sidebar-without-widening-it-references.bib",
                true,
                false
            )
            .await
            .is_err()
        );
        assert_eq!(tab.text(), original);
        let after = bib.set_field(0, "year", "2025").unwrap();
        edit::apply(&tab, original, &after).unwrap();
        glib::timeout_future(Duration::from_millis(250)).await;
        {
            let shown = panel.entries.library.borrow();
            let shown = shown.as_ref().unwrap();
            assert_eq!(shown.value(&shown.entries[0], "year"), "2025");
        }
        panel.entries.view.grab_focus();
        panel
            .entries
            .view
            .activate_action("win.bib-undo", None)
            .unwrap();
        assert_eq!(tab.text(), original);

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
        panel.refresh();
        panel.entries.selection.select_item(0, true);
        panel.entries.view.emit_by_name::<()>("activate", &[&0u32]);
        let at = tab.buffer.iter_at_mark(&tab.buffer.get_insert());
        assert_eq!(at.line(), 1, "activating a reference reveals its source");
        assert!(tab.view.is_drawable());
        glib::timeout_future(Duration::from_millis(400)).await;
        // A filtered row must delete its own entry, not the source caret's reference.
        panel.entries.search.set_text("lee_readout");
        panel.entries.filter();
        glib::timeout_future(Duration::from_millis(250)).await;
        let remove = descendant::<gtk::Button>(&panel.entries.view.clone().upcast(), |b| {
            b.tooltip_text().as_deref() == Some("Remove Reference")
        })
        .expect("a delete button on the reference row");
        let actions = remove
            .parent()
            .unwrap()
            .downcast::<gtk::Revealer>()
            .unwrap();
        let row = actions.parent().unwrap().parent().unwrap();
        row.set_state_flags(gtk::StateFlags::PRELIGHT, false);
        assert!(actions.reveals_child(), "hover reveals the delete action");
        row.unset_state_flags(gtk::StateFlags::PRELIGHT);
        assert!(
            !actions.reveals_child(),
            "leaving the row hides the delete action"
        );
        remove.emit_clicked();
        glib::timeout_future(Duration::from_millis(100)).await;
        let dialog = app.window.visible_dialog().expect("removal review");
        let apply = descendant::<gtk::Button>(&dialog.clone().upcast(), |b| {
            b.label().as_deref() == Some("Apply Changes")
        })
        .unwrap();
        apply.emit_clicked();
        assert!(!tab.text().contains("@article{lee_readout_2025"));
        assert!(tab.text().contains("@article{smith_quantum_2024"));
        assert!(tab.text().contains("@article{garcia_control_2023"));
        assert!(vault.stat("library/paper.pdf").unwrap().is_some());
        tab.buffer.undo();
        panel.entries.search.set_text("");
        panel.refresh();
        glib::timeout_future(Duration::from_millis(350)).await;
        // Long paths must not become the sidebar's minimum width.
        assert!(panel.chooser.measure(gtk::Orientation::Horizontal, -1).0 < 280);
        // The file chooser's search filters its list by path.
        panel.chooser.emit_by_name::<()>("activate", &[]);
        glib::timeout_future(Duration::from_millis(200)).await;
        let chooser = panel.chooser.clone().upcast::<gtk::Widget>();
        let files = descendant::<gtk::ListView>(&chooser, |_| true).unwrap();
        let all = files.model().unwrap().n_items();
        assert!(all >= 3, "three bibliographies listed, got {all}");
        let find = descendant::<gtk::SearchEntry>(&chooser, |_| true).unwrap();
        find.set_text("copied");
        glib::timeout_future(Duration::from_millis(400)).await;
        let found = files.model().unwrap().n_items();
        find.set_text("");
        descendant::<gtk::Popover>(&chooser, |_| true)
            .unwrap()
            .popdown();
        assert_eq!(found, 1, "searching the chooser narrows its list");
        let remove = descendant::<gtk::Button>(&panel.entries.view.clone().upcast(), |b| {
            b.tooltip_text().as_deref() == Some("Remove Reference")
        })
        .unwrap();
        remove
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .set_state_flags(gtk::StateFlags::PRELIGHT, false);
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
        edit::edit(&app, &tab, 0);
        glib::timeout_future(Duration::from_millis(300)).await;
        shot(
            &app.window.clone().upcast(),
            "/tmp/accent-bibliography-edit.png",
        );
        if let Some(dialog) = app.window.visible_dialog() {
            dialog.close();
        }
        app.window.set_default_size(900, 700);
        app.restore_sidebar(true, 240);
        glib::timeout_future(Duration::from_millis(400)).await;
        shot(
            &app.window.clone().upcast(),
            "/tmp/accent-bibliography-narrow.png",
        );
        println!(
            "bench bibliography row_delete=true source_only=true source_preserved=true undo=true sidebar_unchanged=true copy=true move=true stale_review_refused=true chooser_search=true"
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

fn descendant<T: IsA<gtk::Widget> + glib::object::IsClass>(
    widget: &gtk::Widget,
    matches: impl Fn(&T) -> bool + Copy,
) -> Option<T> {
    if let Ok(found) = widget.clone().downcast::<T>()
        && matches(&found)
    {
        return Some(found);
    }
    let mut child = widget.first_child();
    while let Some(next) = child {
        if let Some(found) = descendant(&next, matches) {
            return Some(found);
        }
        child = next.next_sibling();
    }
    None
}
