//! The file menus' drill: a file's tab icon, Reveal in Sidebar into folders not listed yet, a
//! double-click on a tab, and what Open does on a file that is no text.

use super::files::WAIT;
use super::git::xtest;
use super::*;

/// `ACCENT_BENCH_MENU=menus:<rel_note>[:<dir>]` opens the note beside a PDF, an image, a
/// notebook, a diagram and a shell and prints each tab's icon; reveals a file two folders down
/// that the tree never listed; double-clicks a preview's tab with the sidebar on Search, then the
/// kept tab again; opens a PDF through the tree menu's Open, printing the toasts; prints every
/// file menu's labels, a dependency file's tab menu and which of the tree's palette commands
/// apply, copies the note and pastes it into a folder through the palette, and prints a PDF that
/// is not open from the tree; then right-clicks a tab, a tree row and two Git rows, the note
/// changed and `Daily/2024-01-02.md` deleted. With `<dir>` it saves the screen there as PNGs
/// (ImageMagick's `import`). The vault has to be a repository of its own, the generated vault
/// committed.
pub(super) fn bench_menus(app: &Rc<App>, arg: &str) {
    let (note, shots) = match arg.split_once(':') {
        Some((note, dir)) => (note.to_string(), Some(dir.to_string())),
        None => (arg.to_string(), None),
    };
    let app = app.clone();
    glib::spawn_future_local(async move {
        let root = app.root();
        let files = [
            ("menus/inner/deep.md", "# Deep\n"),
            ("menus/b.md", "# B\n"),
            (
                "menus/sheet.drawio",
                "<mxfile><diagram name=\"Page-1\"><mxGraphModel><root><mxCell id=\"0\"/>\
                 <mxCell id=\"1\" parent=\"0\"/></root></mxGraphModel></diagram></mxfile>",
            ),
        ];
        for (rel, text) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
        }
        let tree = app.tree.get().expect("a tree");
        until("menus", "reconciled", WAIT, || app.reconciled.get()).await;

        let opened = [
            note.as_str(),
            "Attachments/paper-0.pdf",
            "Attachments/img-0.png",
            "Notebooks/analysis.ipynb",
            "menus/sheet.drawio",
        ];
        for rel in opened {
            app.open_path(rel);
        }
        app.open_terminal();
        until("menus", "terminal", WAIT, || {
            opened.iter().all(|rel| app.doc_for(rel).is_some())
        })
        .await;
        glib::timeout_future(Duration::from_millis(600)).await;
        for doc in app.docs() {
            let icon = doc.page().icon().and_then(|i| IconExt::to_string(&i));
            println!(
                "bench menus icon {} {}",
                doc.page().title(),
                icon.as_deref().unwrap_or("none")
            );
        }
        // The note alone, the preview to come beside it.
        for doc in app.docs().iter().filter(|doc| doc.key() != note) {
            app.pane().tabs.close_page(doc.page());
        }
        glib::timeout_future(Duration::from_millis(600)).await;

        // Two folders down, neither listed yet: Reveal waits for their listings.
        let deep = "menus/inner/deep.md";
        app.toasts.dismiss_all();
        app.reveal_rel(deep, true);
        let refused = || {
            app.toasts
                .shown()
                .iter()
                .any(|t| t.contains("not in the sidebar"))
        };
        until("menus", "reveal", WAIT, || {
            outlined(tree) == [deep] || refused()
        })
        .await;
        println!(
            "bench menus reveal outlined={:?} toasts={:?}",
            outlined(tree),
            app.toasts.shown()
        );

        // A preview's tab double-clicked keeps it, the kept tab double-clicked reveals it.
        app.open_preview("menus/b.md");
        until("menus", "preview", WAIT, || {
            app.doc_for("menus/b.md").is_some()
        })
        .await;
        glib::timeout_future(Duration::from_millis(600)).await;
        if let Some(sidebar) = app.sidebar.get() {
            sidebar.show_pane("search");
        }
        let showing = || match app.sidebar.get() {
            Some(s) if s.is_showing("files") => "files",
            Some(s) if s.is_showing("search") => "search",
            _ => "other",
        };
        let kept = || {
            app.doc_for("menus/b.md")
                .is_some_and(|doc| app.pane().preview().as_ref() != Some(doc.page()))
        };
        let double = |rel: &str| tab_point(&app, doc::file_name(rel));
        if let Some(at) = double("menus/b.md") {
            xtest(&format!("move {at}; focus; down; up; down; up")).await;
        }
        glib::timeout_future(Duration::from_millis(400)).await;
        println!(
            "bench menus double preview kept={} pane={} outlined={:?}",
            kept(),
            showing(),
            outlined(tree)
        );
        if let Some(at) = double("menus/b.md") {
            xtest(&format!("move {at}; down; up; down; up")).await;
        }
        until("menus", "double click", WAIT, || {
            outlined(tree) == ["menus/b.md"]
        })
        .await;
        let focus = gtk::prelude::GtkWindowExt::focus(&app.window);
        println!(
            "bench menus double kept pane={} outlined={:?} tree_focus={} focus={:?}",
            showing(),
            outlined(tree),
            focus.as_ref().is_some_and(|f| f.is_ancestor(tree.view())),
            focus.map(|f| f.type_().name()),
        );
        shot(&shots, "double").await;
        // The tree menu's Open on a PDF opens it, and says nothing about text.
        let pdf = "Attachments/pages.pdf";
        app.toasts.dismiss_all();
        if let Some(ops) = app.ops() {
            let at = gdk::Rectangle::new(0, 0, 1, 1);
            fileops::context_menu(ops, tree.widget(), Some((pdf, false)), &[], at).popdown();
        }
        let _ = tree
            .widget()
            .activate_action("fileops.open", Some(&pdf.to_variant()));
        until("menus", "pdf", WAIT, || app.doc_for(pdf).is_some()).await;
        glib::timeout_future(Duration::from_millis(600)).await;
        println!(
            "bench menus open pdf opened={} toasts={:?}",
            app.doc_for(pdf).is_some(),
            app.toasts.shown()
        );

        // What each file menu holds: the tree's over a note, a folder and a PDF, then each
        // tab's, the window's own and a changed file's in the Git pane.
        let Some(ops) = app.ops().cloned() else {
            return bench_quit(&app);
        };
        for (rel, dir) in [(note.as_str(), false), ("menus", true), (pdf, false)] {
            let at = gdk::Rectangle::new(0, 0, 1, 1);
            let menu = fileops::context_menu(&ops, tree.widget(), Some((rel, dir)), &[], at);
            println!("bench menus tree {rel} {:?}", labels_of(menu.menu_model()));
            menu.popdown();
        }
        app.open_terminal();
        until("menus", "terminal again", WAIT, || {
            app.docs().iter().any(|doc| doc.terminal().is_some())
        })
        .await;
        let tabs = app.pane().tabs.clone();
        for doc in app.docs() {
            tabs.emit_by_name::<()>("setup-menu", &[&Some(doc.page())]);
            println!(
                "bench menus tab {} {:?}",
                doc.page().title(),
                labels_of(tabs.menu_model())
            );
        }
        tabs.emit_by_name::<()>("setup-menu", &[&None::<adw::TabPage>]);
        let prints = labels_of(app.menu.menu_model())
            .into_iter()
            .filter(|l| l.starts_with("Print") || l.starts_with("Export"))
            .collect::<Vec<_>>();
        println!("bench menus primary prints={prints:?}");

        // Print… from the tree on a PDF that is not open opens it, kept, and prints it.
        let closed = "Attachments/comments.pdf";
        let before = gtk::Window::list_toplevels();
        let _ = tree
            .widget()
            .activate_action("fileops.print", Some(&closed.to_variant()));
        until("menus", "print", WAIT, || app.doc_for(closed).is_some()).await;
        let mut dialog = None;
        let mut busy = None;
        for _ in 0..200 {
            let text = app.statusbar.progress_text();
            busy = busy.or((!text.is_empty()).then_some(text));
            dialog = gtk::Window::list_toplevels()
                .into_iter()
                .find(|w| w.is_visible() && !before.contains(w))
                .and_downcast::<gtk::Window>();
            if dialog.is_some() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        let kept = app
            .doc_for(closed)
            .is_some_and(|doc| app.pane().preview().as_ref() != Some(doc.page()));
        println!(
            "bench menus print opened={} kept={kept} busy={busy:?} dialog={:?} toasts={:?}",
            app.doc_for(closed).is_some(),
            dialog.as_ref().map(|d| d.type_().name()),
            app.toasts.shown()
        );
        if let Some(dialog) = dialog {
            dialog.close();
        }

        // A file inside a dependency tree is only read, from its tab as from its row.
        let dependency = "node_modules/pkg/index.js";
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::write(root.join(dependency), "module.exports = 1;\n").unwrap();
        app.open_path(dependency);
        until("menus", "dependency", WAIT, || {
            app.doc_for(dependency).is_some()
        })
        .await;
        if let Some(doc) = app.doc_for(dependency) {
            tabs.emit_by_name::<()>("setup-menu", &[&Some(doc.page())]);
            println!(
                "bench menus tab dependency {:?}",
                labels_of(tabs.menu_model())
            );
            tabs.emit_by_name::<()>("setup-menu", &[&None::<adw::TabPage>]);
        }

        // The tree's own commands from the palette: which apply here, then Copy File on the
        // selected note and Paste Files into a folder selected after it.
        app.sync_applicable();
        let commands = [
            "open-in-new-window",
            "open-in-terminal",
            "cut-file",
            "copy-file",
            "move-file-to",
            "paste-files",
            "download-file",
            "exclude-folder",
            "reload-folder",
            "trash",
            "delete",
        ];
        let enabled: Vec<_> = commands
            .iter()
            .filter(|name| {
                app.window
                    .lookup_action(name)
                    .is_some_and(|action| action.is_enabled())
            })
            .collect();
        println!("bench menus palette enabled={enabled:?}");
        app.reveal_rel(&note, false);
        until("menus", "reveal note", WAIT, || {
            tree.selected().is_some_and(|row| row.rel == note)
        })
        .await;
        let _ = WidgetExt::activate_action(&app.window, "win.copy-file", None);
        glib::timeout_future(Duration::from_millis(400)).await;
        app.reveal_rel("menus/inner", false);
        until("menus", "reveal folder", WAIT, || {
            tree.selected().is_some_and(|row| row.rel == "menus/inner")
        })
        .await;
        let _ = WidgetExt::activate_action(&app.window, "win.paste-files", None);
        let pasted = root.join("menus/inner").join(doc::file_name(&note));
        until("menus", "paste", WAIT, || pasted.exists()).await;
        println!("bench menus palette pasted={}", pasted.exists());

        // Real right-clicks, as the reader makes them: a tab, a tree row and a Git row.
        let (sx, sy) = app.window.surface_transform();
        let screen =
            |p: graphene::Point| format!("{} {}", f64::from(p.x()) + sx, f64::from(p.y()) + sy);
        if let Some(at) = tab_point(&app, doc::file_name(&note)) {
            right_click(&app, &at, &shots, "tab").await;
        }
        app.reveal_rel(&note, false);
        glib::timeout_future(Duration::from_millis(600)).await;
        if let Some(at) = super::folders::centre(&app, tree, &note) {
            right_click(&app, &screen(at), &shots, "tree").await;
        }
        std::fs::write(root.join(&note), "# Changed\n").unwrap();
        let gone = "Daily/2024-01-02.md";
        std::fs::remove_file(root.join(gone)).unwrap();
        if let Some(sidebar) = app.sidebar.get() {
            sidebar.show_pane("git");
        }
        let list = app.git.get().and_then(|git| git.divider().start_child());
        let row = |path: &str| {
            let list = list.as_ref()?;
            super::git::change_row(list, path)
        };
        until("menus", "changes", WAIT, || {
            row(&note).is_some() && row(gone).is_some()
        })
        .await;
        glib::timeout_future(Duration::from_millis(600)).await;
        for path in [note.as_str(), gone] {
            let at = row(path).and_then(|row| {
                let middle =
                    graphene::Point::new(row.width() as f32 / 3.0, row.height() as f32 / 2.0);
                row.compute_point(&app.window, &middle)
            });
            if let Some(at) = at {
                let name = if path == gone { "git-deleted" } else { "git" };
                right_click(&app, &screen(at), &shots, name).await;
            }
        }
        bench_quit(&app);
    });
}

/// `ACCENT_BENCH_MENU=reveal:<rel>[,<rel>…]` reveals each file in turn, as Reveal in Sidebar does,
/// and prints how far its row's middle lands below the list's (`off`, negative above it) and
/// where the list is scrolled, out of how far it scrolls (`value`, `max`). A `<rel>` after a `!`
/// is revealed with the Search pane in front, so Files comes back for it.
pub(super) fn bench_reveal(app: &Rc<App>, arg: &str) {
    let (app, rels) = (
        app.clone(),
        arg.split(',').map(str::to_string).collect::<Vec<_>>(),
    );
    glib::spawn_future_local(async move {
        let tree = app.tree.get().expect("a tree");
        until("reveal", "reconciled", WAIT, || app.reconciled.get()).await;
        for rel in rels {
            let (searching, rel) = match rel.strip_prefix('!') {
                Some(rel) => (true, rel.to_string()),
                None => (false, rel),
            };
            if let Some(sidebar) = app.sidebar.get().filter(|_| searching) {
                sidebar.show_pane("search");
                glib::timeout_future(Duration::from_millis(300)).await;
            }
            app.reveal_rel(&rel, true);
            until("reveal", "outlined", WAIT, || {
                outlined(tree) == [rel.as_str()]
            })
            .await;
            // Past the frames the list takes to settle under a far jump.
            glib::timeout_future(Duration::from_millis(1000)).await;
            let view = tree.view();
            let a = view.vadjustment().expect("a scrolled tree");
            let row = tree::expanders(view)
                .into_iter()
                .find(|e| {
                    e.list_row()
                        .and_then(|r| tree::decode(&r.item()?))
                        .is_some_and(|r| r.rel == rel)
                })
                .and_then(|e| e.compute_bounds(view));
            let off = row.map(|b| f64::from(b.y() + b.height() / 2.0) - a.page_size() / 2.0);
            println!(
                "bench reveal {rel} off={:?} value={} max={}",
                off.map(f64::round),
                a.value().round(),
                (a.upper() - a.page_size()).round()
            );
        }
        bench_quit(&app);
    });
}

/// Every label a menu model offers, its sections walked through.
fn labels_of(menu: Option<gio::MenuModel>) -> Vec<String> {
    menu.map(|m| fileops::labels(&m)).unwrap_or_default()
}

/// Right-click at `at` on the screen, print the menu that comes up and save the screen as `name`,
/// then close the menu.
async fn right_click(app: &Rc<App>, at: &str, shots: &Option<String>, name: &str) {
    xtest(&format!("move {at}; down 3; up 3")).await;
    let menu = find_widget(app.window.upcast_ref(), &|w| {
        w.is::<gtk::PopoverMenu>() && w.is_visible()
    })
    .and_downcast::<gtk::PopoverMenu>();
    println!(
        "bench menus right_click {name} {:?}",
        labels_of(menu.as_ref().and_then(|m| m.menu_model()))
    );
    shot(shots, name).await;
    if let Some(menu) = menu {
        menu.popdown();
    }
    glib::timeout_future(Duration::from_millis(300)).await;
}

/// The rows the tree outlines as revealed.
fn outlined(tree: &tree::Tree) -> Vec<String> {
    tree::expanders(tree.view())
        .into_iter()
        .filter(|e| e.has_css_class("accent-revealed"))
        .filter_map(|e| Some(tree::decode(&e.list_row()?.item()?)?.rel))
        .collect()
}

/// Where on the screen the active pane's tab titled `title` is, for `xtest`.
fn tab_point(app: &App, title: &str) -> Option<String> {
    let bar = app.pane().bar.clone();
    let tab = find_widget(bar.upcast_ref(), &|w| {
        w.css_name() == "tab"
            && find_widget(w, &|l| {
                l.downcast_ref::<gtk::Label>()
                    .is_some_and(|l| l.label() == title)
            })
            .is_some()
    })?;
    let middle = graphene::Point::new(tab.width() as f32 / 2.0, tab.height() as f32 / 2.0);
    let p = tab.compute_point(&app.window, &middle)?;
    let (sx, sy) = app.window.surface_transform();
    Some(format!(
        "{} {}",
        f64::from(p.x()) + sx,
        f64::from(p.y()) + sy
    ))
}

/// Save the screen as `<dir>/<name>.png` when the drill was given a directory.
async fn shot(dir: &Option<String>, name: &str) {
    let Some(dir) = dir else { return };
    let display = std::env::var("DISPLAY").unwrap_or_default();
    let file = format!("{dir}/{name}.png");
    let argv = ["import", "-display", &display, "-window", "root", &file].map(std::ffi::OsStr::new);
    if let Ok(run) = gio::Subprocess::newv(&argv, gio::SubprocessFlags::NONE) {
        let _ = run.wait_future().await;
    }
}
