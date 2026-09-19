//! Drills over a PDF: the layout either side of Fit Height, the page Add Page appends, the page
//! edits, and the blank document New Drawing writes.

use super::*;

/// Open a PDF, leave the reader halfway down its second page, fit the page from there, and then
/// append one.
///
/// Fit Height and Add Page are fired as the window actions the status bar's menu, the page's own
/// menu and the palette all fire, so a route that never reaches the tab shows up here as a zoom
/// that did not change or a page count that did not grow. The append is followed all the way to
/// the file: the document is re-opened from disk at the end, which is what a second reader sees.
pub(super) fn bench_pdf(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let Some(pdf) = opened(&app, &rel).await else {
            println!("bench pdf no_tab");
            return bench_quit(&app);
        };
        println!("bench pdf pages={} {}", pdf.page_count(), pdf.geometry());
        let page = 1.min(pdf.page_count().saturating_sub(1));
        pdf.scroll_to(pdfview::Anchor {
            page,
            u: 0.0,
            v: 0.5,
        });
        println!("bench pdf mid_page {}", pdf.geometry());
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-fit-page", None);
        println!(
            "bench pdf fit_page {} label={:?}",
            pdf.geometry(),
            pdf.zoom_label()
        );
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-add-page", None);
        written(&app).await;
        let sizes = accent_core::pdf::PdfDoc::open(pdf.path())
            .and_then(|doc| doc.page_sizes())
            .unwrap_or_default();
        println!(
            "bench pdf added pages={} at={} on_disk={:?}",
            pdf.page_count(),
            pdf.place().page,
            sizes
        );
        // What the vault itself holds. On a remote one that is the host's document rather than
        // the cached copy the render thread writes into, so a page that grew here and not there
        // is an upload that never happened.
        println!("bench pdf added in_vault {}", vault_pages(&app, &pdf.key()));
        bench_pdf_renamed(&app).await;
    });
}

/// Page edits end to end: the first page moved below the third by the call a drop in the
/// thumbnail strip makes, a page inserted after the one being read and the one being read deleted
/// through the window actions — the delete's dialog answered by emitting its own `response`, as
/// [`bench_drawing`] answers New Drawing's — each followed to the file. What a headless run cannot
/// reach is the pointer's half: the drag itself, the buttons on hover, the drop bar and the scroll
/// at the strip's edge.
pub(super) fn bench_pdf_pages(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let Some(pdf) = opened(&app, &rel).await else {
            println!("bench pages no_tab");
            return bench_quit(&app);
        };
        println!("bench pages opened {}", pages_read(&pdf));
        pdf.edit_pages(accent_core::pdf::PageEdit::Move { from: 0, to: 2 });
        written(&app).await;
        println!("bench pages moved {}", pages_read(&pdf));
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-insert-page", None);
        written(&app).await;
        println!("bench pages inserted {}", pages_read(&pdf));
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-delete-page", None);
        for _ in 0..40 {
            if app.window.visible_dialog().is_some() {
                break;
            }
            glib::timeout_future(Duration::from_millis(50)).await;
        }
        let Some(dialog) = app
            .window
            .visible_dialog()
            .and_then(|d| d.downcast::<adw::AlertDialog>().ok())
        else {
            println!("bench pages no_dialog");
            return bench_quit(&app);
        };
        println!(
            "bench pages asked heading={:?} body={:?}",
            dialog.heading(),
            dialog.body()
        );
        dialog.emit_by_name::<()>("response", &[&"delete"]);
        written(&app).await;
        println!("bench pages deleted {}", pages_read(&pdf));
        bench_quit(&app);
    });
}

/// The thumbnail strip held on screen for XTEST to hover and drag along: the Outline pane up, and
/// the page being read and the file's pages printed every two seconds for 40 s.
pub(super) fn bench_pdf_strip(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let Some(pdf) = opened(&app, &rel).await else {
            println!("bench strip no_tab");
            return bench_quit(&app);
        };
        app.show_pane("outline");
        for _ in 0..20 {
            println!("bench strip {}", pages_read(&pdf));
            glib::timeout_future(Duration::from_secs(2)).await;
        }
        bench_quit(&app);
    });
}

/// The page being read, and what each page of the file on disk says: what a second reader opens.
fn pages_read(pdf: &pdftab::PdfTab) -> String {
    let text = |doc: &accent_core::pdf::PdfDoc, page| {
        let glyphs = doc.page_text(page).unwrap_or_default();
        glyphs
            .iter()
            .map(|g| g.ch)
            .collect::<String>()
            .trim()
            .to_string()
    };
    let on_disk: Vec<String> = accent_core::pdf::PdfDoc::open(pdf.path())
        .map(|doc| (0..doc.page_count()).map(|p| text(&doc, p)).collect())
        .unwrap_or_default();
    format!(
        "reading={} of {} on_disk={on_disk:?}",
        pdf.current_page() + 1,
        pdf.page_count()
    )
}

/// Open `rel` and hand back the tab once its pages are known.
///
/// Both halves wait: a remote window is up and taking commands well before its host has answered,
/// and the pages are measured on the render thread after a fetch that takes as long as the link
/// does. A local vault passes straight through both.
async fn opened(app: &Rc<App>, rel: &str) -> Option<Rc<pdftab::PdfTab>> {
    for _ in 0..150 {
        if !app.offline() {
            break;
        }
        glib::timeout_future(Duration::from_millis(100)).await;
    }
    app.open_path(rel);
    for _ in 0..150 {
        if let Some(pdf) = app.active_pdf().filter(|pdf| pdf.page_count() > 0) {
            return Some(pdf);
        }
        glib::timeout_future(Duration::from_millis(100)).await;
    }
    None
}

/// Long enough for the tab's own save timer, and then for the upload a remote vault answers it
/// with, so what the drill reads back is the file and not the plan.
async fn written(app: &Rc<App>) {
    glib::timeout_future(Duration::from_millis(1400)).await;
    if app.vault().is_some_and(|v| v.is_remote()) {
        glib::timeout_future(Duration::from_millis(2000)).await;
    }
}

/// The same document under a new name: rename it the way a dropped row does, then append another
/// page and read the file back.
///
/// The render thread owns the path it reloads from and saves to, so a rename it was never told
/// about shows up here as a page count on disk that did not grow — the save going to a name that
/// is no longer there.
async fn bench_pdf_renamed(app: &Rc<App>) {
    let (Some(pdf), Some(ops)) = (app.active_pdf(), app.ops().cloned()) else {
        println!("bench pdf no_tab");
        return bench_quit(app);
    };
    let from = pdf.key();
    let Some(stem) = from.strip_suffix(".pdf") else {
        println!("bench pdf not_a_pdf {from}");
        return bench_quit(app);
    };
    let (to, was) = (format!("{stem}-renamed.pdf"), pdf.path());
    crate::fileops::move_dropped(&ops, &from, &to);
    // The rename runs on a worker and the watcher's event lands a turn after it.
    glib::timeout_future(Duration::from_millis(1500)).await;
    let Some(pdf) = app.active_pdf() else {
        println!("bench pdf no_tab");
        return bench_quit(app);
    };
    println!(
        "bench pdf renamed key={:?} reads={:?} old_gone={}",
        pdf.key(),
        pdf.path().file_name().map(|n| n.to_string_lossy()),
        !was.exists()
    );
    let _ = WidgetExt::activate_action(&app.window, "win.pdf-add-page", None);
    written(app).await;
    let sizes = accent_core::pdf::PdfDoc::open(pdf.path())
        .and_then(|doc| doc.page_sizes())
        .unwrap_or_default();
    println!(
        "bench pdf renamed_added pages={} on_disk={} in_vault {}",
        pdf.page_count(),
        sizes.len(),
        vault_pages(app, &pdf.key())
    );
    bench_quit(app);
}

/// What the vault's own copy of `key` holds, fetched past the cache the reader is drawing on.
fn vault_pages(app: &Rc<App>, key: &str) -> String {
    let Some(vault) = app.vault() else {
        return "no_vault".to_string();
    };
    let dest = std::env::temp_dir().join(format!("accent-bench-{}.pdf", std::process::id()));
    match vault.download(key, &dest) {
        Ok(()) => match accent_core::pdf::PdfDoc::open(&dest).and_then(|d| d.page_sizes()) {
            Ok(sizes) => format!("pages={}", sizes.len()),
            Err(e) => format!("unreadable={e}"),
        },
        Err(e) => format!("download_failed={e}"),
    }
}

/// The etag gate on the way back to a host: a page appended to a document whose host copy has
/// moved since it was fetched must not overwrite it, and the ink must not be dropped either — it
/// goes beside the original in the vault, as `<name> (drawn).pdf`.
///
/// The move is made by stamping the cached copy with an etag the host never had, rather than by
/// really writing on the host: a host-side write is reported by its own watcher, and the refetch
/// that follows wins the race against the save under test every time. What `push` compares is
/// the stamp against the host, so this is the same input from where it stands.
///
/// A second page is appended after the first refusal, which is the reader who keeps drawing: it
/// must write the same copy again rather than a numbered one, and it must not toast again.
pub(super) fn bench_pdf_stale(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let (Some(pdf), Some(vault)) = (opened(&app, &rel).await, app.vault().cloned()) else {
            println!("bench pdf stale no_tab");
            return bench_quit(&app);
        };
        let (key, Some(remote)) = (pdf.key(), vault.remote()) else {
            println!("bench pdf stale not_remote");
            return bench_quit(&app);
        };
        let stamp = accent_api::ssh::stamp_path(remote.url(), &key);
        // An `Etag` as the stamp file spells one, written by hand because the app does not link
        // serde_json: what matters is only that it is not the one the host will report.
        let moved = stamp
            .as_ref()
            .map(|stamp| std::fs::write(stamp, br#"{"mtime_ns":1,"size":1,"ino":1}"#));
        println!(
            "bench pdf stale opened pages={} in_vault {} moved={moved:?}",
            pdf.page_count(),
            vault_pages(&app, &key)
        );
        let kept = pdf.path().with_extension("kept.pdf");
        let (first, second) = (
            accent_api::remote::drawn_name(&key, 1),
            accent_api::remote::drawn_name(&key, 2),
        );
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-add-page", None);
        written(&app).await;
        println!(
            "bench pdf stale refused pages={} in_vault {} drawn {} said={} {:?} kept={}",
            pdf.page_count(),
            vault_pages(&app, &key),
            vault_pages(&app, &first),
            app.toasted.get(),
            bench_said(&app),
            kept.exists()
        );
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-add-page", None);
        written(&app).await;
        println!(
            "bench pdf stale again pages={} in_vault {} drawn {} said={} numbered={}",
            pdf.page_count(),
            vault_pages(&app, &key),
            vault_pages(&app, &first),
            app.toasted.get(),
            vault.exists(&second)
        );
        let _ = std::fs::remove_file(&kept);
        bench_quit(&app);
    });
}

/// New Drawing end to end, as far as a headless run reaches: fire the window action, read what
/// the dialog came up with, pick the last size and answer it, then say what reached the disk and
/// what the tab it opened is holding.
///
/// The dialog is answered by emitting its own `response` signal rather than by pressing its
/// button: Xvfb has no window manager, the toplevel never becomes active and a click never
/// reaches an `AdwAlertDialog`'s buttons. What this covers is everything the button leads to —
/// the handler, the document, the file, the tab and the tool in hand; the button itself is
/// libadwaita's.
pub(super) fn bench_drawing(app: &Rc<App>) {
    let app = app.clone();
    glib::spawn_future_local(async move {
        let _ = WidgetExt::activate_action(&app.window, "win.new-drawing", None);
        for _ in 0..40 {
            if app.window.visible_dialog().is_some() {
                break;
            }
            glib::timeout_future(Duration::from_millis(50)).await;
        }
        let Some(dialog) = app
            .window
            .visible_dialog()
            .and_then(|d| d.downcast::<adw::AlertDialog>().ok())
        else {
            println!("bench drawing no_dialog");
            return bench_quit(&app);
        };
        let form = dialog.extra_child();
        let typed = form
            .as_ref()
            .and_then(|form| find_widget(form, &|w| w.is::<gtk::Entry>()))
            .and_downcast::<gtk::Entry>()
            .map(|entry| entry.text());
        let sizes = form
            .as_ref()
            .and_then(|form| find_widget(form, &|w| w.is::<gtk::DropDown>()))
            .and_downcast::<gtk::DropDown>();
        let listed = sizes.as_ref().and_then(|d| d.model()).map(|m| m.n_items());
        println!(
            "bench drawing heading={:?} typed={typed:?} sizes={listed:?}",
            dialog.heading()
        );
        // The last shape, which is the one with arithmetic behind it: the window's proportions.
        let Some(sizes) = sizes else {
            println!("bench drawing no_sizes");
            return bench_quit(&app);
        };
        sizes.set_selected(3);
        dialog.emit_by_name::<()>("response", &[&"confirm"]);
        glib::timeout_future(Duration::from_millis(800)).await;
        let Some(pdf) = app.active_pdf() else {
            println!("bench drawing no_tab");
            return bench_quit(&app);
        };
        let sizes = accent_core::pdf::PdfDoc::open(pdf.path())
            .and_then(|doc| doc.page_sizes())
            .unwrap_or_default();
        println!(
            "bench drawing made key={:?} tool={:?} window={}x{} on_disk={sizes:?}",
            pdf.key(),
            pdf.mode_label(),
            app.window.width(),
            app.window.height(),
        );
        bench_quit(&app);
    });
}
