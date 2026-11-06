//! Opens that fail, that turn out to be another kind, and that are asked for again while their
//! read is in flight: what waited on each, and what the window said.

use super::*;

/// `ACCENT_BENCH_TABS=opens`: work for a text tab asked of a file gone from the vault, of one
/// that cannot be read (a folder named as a note, which reads for nobody, root included; a tree
/// double-click waiting on it too), of a binary file and of an `.xml` draw.io wrote. After each it
/// prints what was said, whether the work ran, how much still waits and what tab the file has.
/// Then a note looked at, asked for work, opened again and double-clicked, all before its read
/// lands, and the unreadable one once it is a note: each lands once, kept only where the double
/// click still stood. Open as Text and Open in Accent's Editor are asked along with each open,
/// again of the note once it is open, and of a plain `.py` a search hit opens, and `once` says
/// whether they still stand after it: they go with the open they were asked of, so a notebook
/// that could not be read when Open as Text asked opens as a notebook next time. Writes its files
/// into the vault root.
pub(super) fn bench_opens(app: &Rc<App>) {
    let app = app.clone();
    glib::spawn_future_local(async move {
        let root = app.root();
        let write = |rel: &str, bytes: &[u8]| std::fs::write(root.join(rel), bytes).is_ok();
        let diagram = b"<mxfile><diagram name=\"First\" id=\"First\"><mxGraphModel><root>\
            <mxCell id=\"0\"/></root></mxGraphModel></diagram></mxfile>";
        let wrote = std::fs::create_dir(root.join("opens-unread.md")).is_ok()
            && write("opens-blob.txt", b"blob\0\x01\x02")
            && write("opens-diagram.xml", diagram)
            && write("opens-note.md", b"# Note\n")
            && write("opens-plain.py", b"x = 1\n")
            && std::fs::create_dir(root.join("opens-unread.ipynb")).is_ok();
        if !wrote {
            println!("bench opens wrote=false");
            return bench_quit(&app);
        }
        glib::timeout_future(Duration::from_millis(400)).await;
        for (case, rel) in [
            ("gone", "opens-gone.md"),
            ("unreadable", "opens-unread.md"),
            ("binary", "opens-blob.txt"),
            ("diagram", "opens-diagram.xml"),
        ] {
            app.toasts.dismiss_all();
            if case == "unreadable" {
                app.pending.keep(rel);
            }
            app.pending.as_text(rel);
            app.pending.in_accent(rel);
            let ran = Rc::new(Cell::new(false));
            let runs = ran.clone();
            app.with_tab(rel, Opened::Kept, "go to", move |_, _| runs.set(true));
            until(|| !app.toasts.shown().is_empty()).await;
            glib::timeout_future(Duration::from_millis(300)).await;
            println!(
                "bench opens {case} said={:?} ran={} waiting={} tab={} once={:?}",
                said(&app),
                ran.get(),
                app.pending.waiting().len(),
                super::terminal_editor::kind_of(&app, rel),
                app.pending.once(rel)
            );
        }
        app.toasts.dismiss_all();
        let note = "opens-note.md";
        let ran = Rc::new(Cell::new(false));
        let runs = ran.clone();
        app.pending.as_text(note);
        app.pending.in_accent(note);
        app.open_preview(note);
        app.with_tab(note, Opened::Kept, "go to", move |_, _| runs.set(true));
        app.open_path(note);
        app.pending.keep(note);
        until(|| ran.get()).await;
        // The reads asked for after the first, which find its tab when they land.
        glib::timeout_future(Duration::from_millis(1000)).await;
        println!(
            "bench opens during said={:?} ran={} waiting={} {} once={:?}",
            said(&app),
            ran.get(),
            app.pending.waiting().len(),
            landed(&app, note),
            app.pending.once(note)
        );
        // Asked of the note open already, whose tab comes to the front.
        app.pending.as_text(note);
        app.pending.in_accent(note);
        app.open_path(note);
        println!("bench opens again once={:?}", app.pending.once(note));
        // A search hit in a `.py`, which waits for the file to say whether it is a notebook.
        let py = "opens-plain.py";
        app.pending.as_text(py);
        app.pending.in_accent(py);
        app.open_note_at(py, Some(sidebar::Target::Range(0..1, None)));
        until(|| app.doc_for(py).is_some()).await;
        let tab = super::terminal_editor::kind_of(&app, py);
        println!("bench opens hit tab={tab} once={:?}", app.pending.once(py));
        app.toasts.dismiss_all();
        let notebook = "opens-unread.ipynb";
        app.pending.as_text(notebook);
        app.open_path(notebook);
        until(|| !app.toasts.shown().is_empty()).await;
        let once = app.pending.once(notebook);
        println!("bench opens as_text said={:?} once={once:?}", said(&app));
        let cells = concat!(
            r#"{"cells": [{"cell_type": "markdown", "id": "a", "metadata": {}, "source": ["A"]}], "#,
            r#""metadata": {}, "nbformat": 4, "nbformat_minor": 5}"#
        );
        let path = root.join(notebook);
        let _ = std::fs::remove_dir(&path).and_then(|()| std::fs::write(&path, cells));
        app.open_preview(notebook);
        until(|| app.doc_for(notebook).is_some()).await;
        let tab = super::terminal_editor::kind_of(&app, notebook);
        println!("bench opens as_text_next tab={tab}");
        let unread = root.join("opens-unread.md");
        let _ = std::fs::remove_dir(&unread).and_then(|()| std::fs::write(&unread, "# Read\n"));
        app.open_preview("opens-unread.md");
        until(|| app.doc_for("opens-unread.md").is_some()).await;
        println!("bench opens readable {}", landed(&app, "opens-unread.md"));
        bench_quit(&app);
    });
}

/// The toasts standing, the vault's path in them spelt `<vault>`: a drill's is a scratch folder.
fn said(app: &Rc<App>) -> Vec<String> {
    let root = app.root().display().to_string();
    let shown = app.toasts.shown().into_iter();
    shown.map(|toast| toast.replace(&root, "<vault>")).collect()
}

/// How many tabs `rel` has across the panes, and whether its tab is the pane's preview.
fn landed(app: &Rc<App>, rel: &str) -> String {
    let doc = app.doc_for(rel);
    let panes = app.panes.borrow().clone();
    let of = |page: &adw::TabPage| app.doc_for_page(page).is_some_and(|d| d.key() == rel);
    let tabs: usize = panes
        .iter()
        .map(|pane| pane.pages().iter().filter(|p| of(p)).count())
        .sum();
    let preview =
        doc.and_then(|doc| Some(app.pane_of(doc.page())?.preview().as_ref() == Some(doc.page())));
    format!("tabs={tabs} preview={preview:?}")
}

/// Wait until `done`, or five seconds.
async fn until(done: impl Fn() -> bool) {
    for _ in 0..100 {
        if done() {
            return;
        }
        glib::timeout_future(Duration::from_millis(50)).await;
    }
}
