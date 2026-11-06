//! Opens that fail, that turn out to be another kind, and that are asked for again while their
//! read is in flight: what waited on each, and what the window said.

use super::*;

/// `ACCENT_BENCH_TABS=opens`: work for a text tab asked of a file gone from the vault, of one
/// that cannot be read (a folder named as a note, which reads for nobody, root included; a tree
/// double-click waiting on it too), of a binary file and of an `.xml` draw.io wrote. After each it
/// prints what was said, whether the work ran, how much still waits and what tab the file has.
/// Then a note looked at, asked for work, opened again and double-clicked, all before its read
/// lands, and the unreadable one once it is a note: each lands once, kept only where the double
/// click still stood. Writes its files into the vault root.
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
            && write("opens-note.md", b"# Note\n");
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
                app.keep_on_landing.borrow_mut().insert(rel.to_string());
            }
            let ran = Rc::new(Cell::new(false));
            let runs = ran.clone();
            app.with_tab(rel, Opened::Kept, "go to", move |_, _| runs.set(true));
            until(|| !app.toasts.shown().is_empty()).await;
            glib::timeout_future(Duration::from_millis(300)).await;
            println!(
                "bench opens {case} said={:?} ran={} waiting={} tab={}",
                said(&app),
                ran.get(),
                app.awaiting.borrow().len(),
                super::terminal_editor::kind_of(&app, rel)
            );
        }
        app.toasts.dismiss_all();
        let note = "opens-note.md";
        let ran = Rc::new(Cell::new(false));
        let runs = ran.clone();
        app.open_preview(note);
        app.with_tab(note, Opened::Kept, "go to", move |_, _| runs.set(true));
        app.open_path(note);
        app.keep_on_landing.borrow_mut().insert(note.to_string());
        until(|| ran.get()).await;
        // The reads asked for after the first, which find its tab when they land.
        glib::timeout_future(Duration::from_millis(1000)).await;
        println!(
            "bench opens during said={:?} ran={} waiting={} {}",
            said(&app),
            ran.get(),
            app.awaiting.borrow().len(),
            landed(&app, note)
        );
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
