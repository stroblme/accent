//! Exercise the real bibliography surfaces and save path in a scratch vault.
use super::*;
use accent_core::bib::Attachment;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

/// Rows the reference list built and bound, and reads of the pane's file.
pub static SETUPS: AtomicUsize = AtomicUsize::new(0);
pub static BINDS: AtomicUsize = AtomicUsize::new(0);
pub static READS: AtomicUsize = AtomicUsize::new(0);

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
        if let Some(rel) = std::env::var("ACCENT_BENCH_BIBLIOGRAPHY")
            .ok()
            .and_then(|v| v.strip_prefix("time:").map(str::to_string))
        {
            time(&app, &rel).await;
            return app.window.application().unwrap().quit();
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
        let rows = || {
            (0..4)
                .map(|i| panel.entries.model.item(i))
                .collect::<Vec<_>>()
        };
        let kept = rows();
        edit::apply(&tab, original, &after).unwrap();
        glib::timeout_future(Duration::from_millis(250)).await;
        {
            let shown = panel.entries.library.borrow();
            let shown = shown.as_ref().unwrap();
            assert_eq!(shown.value(&shown.entries[0], "year"), "2025");
        }
        let now = rows();
        assert!(
            now[0] != kept[0] && now[1..] == kept[1..],
            "an edited reference replaces its own row alone"
        );
        panel.entries.view.grab_focus();
        panel
            .entries
            .view
            .activate_action("win.bib-undo", None)
            .unwrap();
        assert_eq!(tab.text(), original);
        // A refresh that finds the file as it was keeps the rows, whose widgets cost a large
        // bibliography a second of the main loop to build again.
        glib::timeout_future(Duration::from_millis(250)).await;
        let first = panel.entries.model.item(0);
        panel.refresh();
        assert_eq!(
            panel.entries.model.item(0),
            first,
            "an unchanged bibliography keeps its rows"
        );

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
            .ancestor(gtk::Revealer::static_type())
            .and_downcast::<gtk::Revealer>()
            .unwrap();
        let row = actions.parent().unwrap().parent().unwrap();
        row.set_state_flags(gtk::StateFlags::PRELIGHT, false);
        assert!(actions.reveals_child(), "hover reveals the delete action");
        row.unset_state_flags(gtk::StateFlags::PRELIGHT);
        assert!(
            !actions.reveals_child(),
            "leaving the row hides the delete action"
        );
        // The row's copy button and menu act on its own reference; the header's menu on the file.
        let copy = descendant::<gtk::Button>(&row, |b| {
            b.tooltip_text().as_deref() == Some("Copy Citation Key")
        })
        .expect("a copy button on the reference row");
        copy.emit_clicked();
        let copied = app.window.clipboard().read_text_future().await.unwrap();
        assert_eq!(copied.as_deref(), Some("lee_readout_2025"));
        let has = |button: &gtk::MenuButton, action: &str| {
            let menu = button.menu_model().unwrap();
            (0..menu.n_items()).any(|i| {
                let section = menu.item_link(i, "section").unwrap();
                (0..section.n_items()).any(|j| {
                    section
                        .item_attribute_value(j, "action", None)
                        .and_then(|v| v.get::<String>())
                        .as_deref()
                        == Some(action)
                })
            })
        };
        // A menu is built as it opens: a large bibliography's rows each built one up front.
        let more = descendant::<gtk::MenuButton>(&row, |_| true).unwrap();
        assert!(more.menu_model().is_none(), "a row's menu waits to open");
        row.set_state_flags(gtk::StateFlags::PRELIGHT, false);
        more.popup();
        assert!(has(&more, "row.edit") && has(&more, "row.download"));
        more.popdown();
        row.unset_state_flags(gtk::StateFlags::PRELIGHT);
        let header = descendant::<gtk::MenuButton>(&panel.root.clone().upcast(), |b| {
            b.tooltip_text().as_deref() == Some("Bibliography Actions")
        })
        .unwrap();
        header.popup();
        assert!(has(&header, "win.bib-check") && !has(&header, "win.bib-edit"));
        header.popdown();
        remove.activate_action("row.edit", None).unwrap();
        glib::timeout_future(Duration::from_millis(100)).await;
        let dialog = app.window.visible_dialog().expect("edit reference");
        assert!(
            descendant::<gtk::Label>(&dialog.clone().upcast(), |l| l
                .text()
                .ends_with("· lee_readout_2025"))
            .is_some(),
            "the row menu edits its own reference"
        );
        dialog.close();
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
            .ancestor(gtk::Revealer::static_type())
            .and_then(|r| r.parent())
            .and_then(|r| r.parent())
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
        // Add Reference downloads the new reference's PDF by itself, the switch on by default.
        assert!(app.config.borrow().bibliography_download_pdf);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/auto.pdf", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") && stream.read_exact(&mut byte).is_ok() {
                request.push(byte[0]);
            }
            let body = "%PDF-1.4\n% downloaded on add\n";
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        });
        app.review_import(
            &tab.rel(),
            "@article{auto,title={Automatic Download},author={Doe, Ada},year=2026}".into(),
            true,
            url,
        );
        glib::timeout_future(Duration::from_millis(200)).await;
        let dialog = app.window.visible_dialog().expect("reference review");
        descendant::<gtk::Button>(&dialog.clone().upcast(), |b| {
            b.label().as_deref() == Some("Add Reference")
        })
        .unwrap()
        .emit_clicked();
        let mut attached = None;
        for _ in 0..100 {
            let bib = Library::parse(&tab.text()).unwrap();
            attached = bib
                .entries
                .iter()
                .find(|e| bib.value(e, "title") == "Automatic Download")
                .and_then(|e| Attachment::parse(&bib.value(e, "file")).ok())
                .and_then(|files| files.into_iter().next());
            if attached.is_some() {
                break;
            }
            glib::timeout_future(Duration::from_millis(50)).await;
        }
        let pdf = bib::attachment_path(&tab.rel(), &attached.expect("PDF attached").path).unwrap();
        assert!(vault.read(&pdf).unwrap().0.starts_with("%PDF-"));
        // The pane reads its file again for a change made elsewhere to it, and for no other file.
        panel.select("project/references.bib");
        vault.write_file("note.md", b"# Note\n").unwrap();
        app.open_path("note.md");
        let note = wait_tab(&app, "note.md").await;
        glib::timeout_future(Duration::from_millis(500)).await;
        let reads = READS.load(Relaxed);
        std::fs::write(app.root().join("note.md"), "# Note\n\nEdited elsewhere.\n").unwrap();
        until(|| note.text().contains("Edited elsewhere")).await;
        glib::timeout_future(Duration::from_millis(300)).await;
        assert_eq!(
            READS.load(Relaxed),
            reads,
            "another file's change reads nothing"
        );
        let path = app.root().join("project/references.bib");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("@misc{elsewhere,title={Edited Elsewhere}}\n");
        std::fs::write(&path, text).unwrap();
        until(|| {
            (panel.entries.library.borrow().as_ref())
                .is_some_and(|b| b.source.contains("Edited Elsewhere"))
        })
        .await;
        println!(
            "bench bibliography row_delete=true source_only=true source_preserved=true undo=true sidebar_unchanged=true copy=true move=true stale_review_refused=true chooser_search=true row_actions=true download_on_add=true own_events=true"
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
/// Wait up to five seconds for `done`.
async fn until(done: impl Fn() -> bool) {
    for _ in 0..100 {
        if done() {
            return;
        }
        glib::timeout_future(Duration::from_millis(50)).await;
    }
    panic!("timed out")
}

/// `time:<rel>`: what a real change of the bibliography `rel` costs the main loop with the pane
/// hidden, shown and hidden again: one reference edited, a search typed and cleared.
async fn time(app: &Rc<App>, rel: &str) {
    let panel = app.bibliography.get().unwrap();
    // The drill runs the search itself, rather than the entry a delay later.
    panel.entries.search.set_search_delay(u32::MAX);
    app.show_pane("files");
    app.open_path(rel);
    let tab = wait_tab(app, rel).await;
    glib::timeout_future(Duration::from_millis(500)).await;
    for (round, pane) in ["files", "bibliography", "files"].into_iter().enumerate() {
        app.show_pane(pane);
        glib::timeout_future(Duration::from_millis(500)).await;
        let before = tab.text();
        let year = (1990 + round).to_string();
        let after = Library::parse(&before)
            .unwrap()
            .set_field(1, "year", &year)
            .unwrap();
        edit::apply(&tab, &before, &after).unwrap();
        panel.changed.cancel();
        measure(&format!("edit pane={pane}"), || panel.refresh()).await;
        panel.entries.search.set_text("key1");
        measure(&format!("search pane={pane}"), || panel.entries.filter()).await;
        measure(&format!("clear pane={pane}"), || {
            panel.entries.search.set_text("")
        })
        .await;
    }
}

/// Print the rows `change` built and bound, its own time and the time until the main loop is
/// idle again, which includes dropping the rows it replaced.
async fn measure(what: &str, change: impl FnOnce()) {
    let (setups, binds) = (SETUPS.load(Relaxed), BINDS.load(Relaxed));
    let start = Instant::now();
    change();
    let ran = start.elapsed();
    let idle = Rc::new(Cell::new(None));
    let done = idle.clone();
    glib::idle_add_local_once(move || done.set(Some(start.elapsed())));
    until(|| idle.get().is_some()).await;
    println!(
        "bench bibliography time {what} setups={} binds={} ran_ms={} idle_ms={}",
        SETUPS.load(Relaxed) - setups,
        BINDS.load(Relaxed) - binds,
        ran.as_millis(),
        idle.get().unwrap().as_millis()
    );
    glib::timeout_future(Duration::from_millis(300)).await;
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
