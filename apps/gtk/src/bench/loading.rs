//! Tabs that are up before their files arrive (DESIGN.md, Loading), on a slow host that
//! [`SLOW_READ_MS`] stands in for.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

/// How long each read and fetch holds on its worker, as a slow host's round trip would:
/// `ACCENT_BENCH_SLOW_READ`, and what `ACCENT_BENCH_TABS=loading:` sets as it runs.
pub(crate) static SLOW_READ_MS: AtomicU64 = AtomicU64::new(0);

/// A one-page diagram and a one-cell notebook, as small as each reads.
const DIAGRAM: &[u8] = b"<mxfile><diagram name=\"First\" id=\"First\"><mxGraphModel><root>\
    <mxCell id=\"0\"/></root></mxGraphModel></diagram></mxfile>";
const NOTEBOOK: &[u8] = br#"{"cells": [{"cell_type": "markdown", "id": "a", "metadata": {}, "source": ["A"]}], "metadata": {}, "nbformat": 4, "nbformat_minor": 5}"#;

thread_local! {
    /// Whether this process has reloaded its window, so the window that comes back looks at the
    /// tabs it restored rather than reloading again.
    static RELOADED: Cell<bool> = const { Cell::new(false) };
}

/// `ACCENT_BENCH_TABS=loading`, each read and fetch held 800 ms: a note's, a PDF's, an image's,
/// a diagram's and a notebook's tab is up, titled and with its icon, as its open is asked, shows
/// the loading page once 160 ms have passed and its document once it lands. Then a file opened as
/// a preview, double-clicked and opened again while on its way (one tab, kept), one closed on its
/// way (no tab, nothing written), a quick open (no loading page), and a folder named as a note:
/// its tab says it cannot open, with Retry, what waited on it is told, and Retry opens the note
/// written there since. Last Reload Window, whose restored tabs are all up at once, in their
/// order, before any lands. Writes `loading*` files into the vault root.
pub(super) fn bench_loading(app: &Rc<App>) {
    let app = app.clone();
    if RELOADED.replace(true) {
        return restored(app);
    }
    glib::spawn_future_local(async move {
        let root = app.root();
        let pixels = glib::Bytes::from_static(&[255; 16]);
        let png = gdk::MemoryTexture::new(2, 2, gdk::MemoryFormat::R8g8b8a8, &pixels, 8);
        let pdf = accent_core::pdf::blank_pdf((595.0, 842.0)).unwrap_or_default();
        let files: [(&str, &[u8]); 8] = [
            ("loading-note.md", b"# Note\n"),
            ("loading.pdf", &pdf),
            ("loading.png", &png.save_to_png_bytes()),
            ("loading.drawio", DIAGRAM),
            ("loading.ipynb", NOTEBOOK),
            ("loading-twice.md", b"# Twice\n"),
            ("loading-close.md", b"# Close\n"),
            ("loading-fast.md", b"# Fast\n"),
        ];
        let wrote = files
            .iter()
            .all(|(rel, bytes)| std::fs::write(root.join(rel), bytes).is_ok())
            && std::fs::create_dir(root.join("loading-fail.md")).is_ok();
        if !wrote {
            println!("bench loading wrote=false");
            return bench_quit(&app);
        }
        glib::timeout_future(Duration::from_millis(400)).await;
        SLOW_READ_MS.store(800, Ordering::Relaxed);
        for (rel, _) in &files[..5] {
            app.open_path(rel);
            let page = app.doc_for(rel).map(|doc| doc.page().clone());
            let title = page.as_ref().map(|page| page.title().to_string());
            let icon = page
                .and_then(|page| page.icon())
                .and_downcast::<gio::ThemedIcon>();
            let icon = icon.map(|icon| icon.names()[0].to_string());
            println!(
                "bench loading open {rel} now={} title={title:?} icon={icon:?}",
                shows(&app, rel)
            );
            glib::timeout_future(Duration::from_millis(300)).await;
            println!("bench loading open {rel} waiting={}", shows(&app, rel));
            until("loading", rel, 10_000, || landed(&app, rel)).await;
            println!("bench loading open {rel} landed={}", shows(&app, rel));
        }

        let rel = "loading-twice.md";
        app.open_preview(rel);
        // Its row double-clicked, as the Files tree does to the tab of the file it opened.
        if let Some(doc) = app.doc_for(rel) {
            app.promote(doc.page());
        }
        app.open_path(rel);
        until("loading", "twice", 10_000, || landed(&app, rel)).await;
        println!(
            "bench loading twice {} {}",
            shows(&app, rel),
            tabs(&app, rel)
        );

        let rel = "loading-close.md";
        let stamp = || {
            std::fs::metadata(root.join(rel))
                .and_then(|m| m.modified())
                .ok()
        };
        let before = stamp();
        app.open_path(rel);
        if let Some(doc) = app.doc_for(rel) {
            app.close_page(doc.page());
        }
        glib::timeout_future(Duration::from_millis(1200)).await;
        let unwritten = before.is_some() && stamp() == before;
        println!(
            "bench loading closed {} unwritten={unwritten}",
            tabs(&app, rel)
        );

        SLOW_READ_MS.store(0, Ordering::Relaxed);
        let shown = crate::loading::SHOWN.load(Ordering::Relaxed);
        let rel = "loading-fast.md";
        app.open_path(rel);
        until("loading", "fast", 10_000, || landed(&app, rel)).await;
        glib::timeout_future(Duration::from_millis(300)).await;
        let flashed = crate::loading::SHOWN.load(Ordering::Relaxed) != shown;
        println!(
            "bench loading fast {} loading_shown={flashed}",
            shows(&app, rel)
        );

        SLOW_READ_MS.store(300, Ordering::Relaxed);
        let rel = "loading-fail.md";
        app.toasts.dismiss_all();
        let ran = Rc::new(Cell::new(false));
        let runs = ran.clone();
        app.with_tab(rel, Opened::Kept, "go to", move |_, _| runs.set(true));
        until(
            "loading",
            "failed",
            10_000,
            || matches!(app.doc_for(rel), Some(Doc::Loading(held)) if held.failed()),
        )
        .await;
        println!(
            "bench loading failed {} said={:?} retry={}",
            shows(&app, rel),
            said(&app),
            retry(&app, rel).is_some()
        );
        let path = root.join(rel);
        let _ = std::fs::remove_dir(&path).and_then(|()| std::fs::write(&path, "# Mended\n"));
        if let Some(button) = retry(&app, rel) {
            button.emit_clicked();
        }
        glib::timeout_future(Duration::from_millis(100)).await;
        println!("bench loading retrying {}", shows(&app, rel));
        until("loading", "retried", 10_000, || landed(&app, rel)).await;
        println!(
            "bench loading retried {} ran={} {}",
            shows(&app, rel),
            ran.get(),
            tabs(&app, rel)
        );

        SLOW_READ_MS.store(800, Ordering::Relaxed);
        println!("bench loading reload {}", bar(&app));
        let _ = WidgetExt::activate_action(&app.window, "win.reload-window", None);
    });
}

/// `ACCENT_BENCH_TABS=opening:<rel>,…`, on a remote vault once its first walk is in: each file
/// opened as a preview, and how long its tab took to be up and its document to land, and the
/// longest the main loop went without a turn meanwhile. Times only.
pub(super) fn bench_opening(app: &Rc<App>, rels: &str) {
    let app = app.clone();
    let rels: Vec<String> = rels.split(',').map(str::to_string).collect();
    glib::spawn_future_local(async move {
        until("opening", "reconciled", 60_000, || app.reconciled.get()).await;
        for rel in &rels {
            let t = Instant::now();
            let (stall, last) = (Rc::new(Cell::new(Duration::ZERO)), Cell::new(t));
            let longest = stall.clone();
            let tick = glib::timeout_add_local(Duration::from_millis(5), move || {
                let now = Instant::now();
                longest.set(longest.get().max(now - last.replace(now)));
                glib::ControlFlow::Continue
            });
            app.open_preview(rel);
            let at_once = app.doc_for(rel).is_some();
            let tab = settle(|| app.doc_for(rel).is_some()).await;
            let doc = settle(|| landed(&app, rel)).await;
            tick.remove();
            println!(
                "bench opening {rel} at_once={at_once} tab_ms={} doc_ms={} stall_ms={}",
                (tab - t).as_millis(),
                (doc - t).as_millis(),
                stall.get().as_millis()
            );
        }
        bench_quit(&app);
    });
}

/// When `done` first held, looked at every 5 ms for a minute.
async fn settle(done: impl Fn() -> bool) -> Instant {
    let t = Instant::now();
    while !done() && t.elapsed() < Duration::from_secs(60) {
        glib::timeout_future(Duration::from_millis(5)).await;
    }
    Instant::now()
}

/// The window Reload Window brought back: its tabs straight after the restore, and once each
/// has landed.
fn restored(app: Rc<App>) {
    glib::spawn_future_local(async move {
        // Once the restore has run, which a remote vault's waits for its host, and past the
        // loading pages' delay, well before the first read lands.
        until("loading", "restored", 60_000, || app.restored.get()).await;
        glib::timeout_future(Duration::from_millis(300)).await;
        println!("bench loading restored {}", bar(&app));
        until("loading", "restored landed", 30_000, || {
            app.docs().iter().all(|doc| landed(&app, &doc.key()))
        })
        .await;
        println!("bench loading restored landed {}", bar(&app));
        SLOW_READ_MS.store(0, Ordering::Relaxed);
        bench_quit(&app);
    });
}

/// What `rel`'s tab is and shows: its document's kind, and the page its stack has up (`-` for
/// a tab with none).
fn shows(app: &Rc<App>, rel: &str) -> String {
    let Some(doc) = app.doc_for(rel) else {
        return "none".to_string();
    };
    let kind = match &doc {
        Doc::Loading(_) => "loading",
        Doc::Text(_) => "text",
        Doc::Pdf(_) => "pdf",
        Doc::Image(_) => "image",
        Doc::Diagram(_) => "diagram",
        Doc::Notebook(_) => "notebook",
        Doc::Status(_) => "status",
        _ => "other",
    };
    let stack = doc.page().child().downcast::<gtk::Stack>().ok();
    let up = stack.and_then(|stack| stack.visible_child_name());
    format!("{kind}/{}", up.as_deref().unwrap_or("-"))
}

/// Whether `rel`'s document is in: read into its tab, a PDF's pages known, an image drawn.
fn landed(app: &Rc<App>, rel: &str) -> bool {
    match app.doc_for(rel) {
        Some(Doc::Loading(_)) | None => false,
        Some(Doc::Pdf(pdf)) => !pdf.opening(),
        Some(Doc::Image(image)) => image.size.get().is_some(),
        Some(_) => true,
    }
}

/// How many tabs `rel` has across the panes, and whether its tab is its pane's preview.
fn tabs(app: &Rc<App>, rel: &str) -> String {
    let panes = app.panes.borrow().clone();
    let of = |page: &adw::TabPage| app.doc_for_page(page).is_some_and(|d| d.key() == rel);
    let count: usize = panes
        .iter()
        .map(|pane| pane.pages().iter().filter(|p| of(p)).count())
        .sum();
    let preview = app.doc_for(rel).and_then(|doc| {
        let pane = app.pane_of(doc.page())?;
        Some(pane.preview().as_ref() == Some(doc.page()))
    });
    format!("tabs={count} preview={preview:?}")
}

/// Each pane's tabs in bar order, as `name:kind/page`, the one in front marked `*`.
fn bar(app: &Rc<App>) -> String {
    let panes = app.panes.borrow().clone();
    let tabs = panes.iter().map(|pane| {
        let front = pane.tabs.selected_page();
        let tabs: Vec<String> = pane
            .pages()
            .iter()
            .filter_map(|page| {
                let key = app.doc_for_page(page)?.key();
                let mark = if front.as_ref() == Some(page) {
                    "*"
                } else {
                    ""
                };
                Some(format!(
                    "{mark}{}:{}",
                    doc::file_name(&key),
                    shows(app, &key)
                ))
            })
            .collect();
        format!("[{}]", tabs.join(" "))
    });
    tabs.collect::<Vec<_>>().join(" ")
}

/// The Retry button on `rel`'s tab, if it says it cannot open.
fn retry(app: &Rc<App>, rel: &str) -> Option<gtk::Button> {
    let page = app.doc_for(rel)?.page().child();
    let found = find_widget(&page, &|w| {
        w.downcast_ref::<gtk::Button>()
            .is_some_and(|b| b.label().as_deref() == Some("Retry"))
    });
    found.and_downcast()
}

/// The toasts standing, the vault's path in them spelt `<vault>`: a drill's is a scratch folder.
fn said(app: &Rc<App>) -> Vec<String> {
    let root = app.root().display().to_string();
    let shown = app.toasts.shown().into_iter();
    shown.map(|toast| toast.replace(&root, "<vault>")).collect()
}
