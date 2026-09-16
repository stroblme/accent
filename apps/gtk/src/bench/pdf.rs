//! Drills over a PDF: the layout either side of Fit Height, the page Add Page appends, and the
//! blank document New Drawing writes.

use super::*;

/// Open a PDF, leave the reader halfway down its second page, fit the page from there, and then
/// append one.
///
/// Fit Height and Add Page are fired as the window actions the status bar's menu, the page's own
/// menu and the palette all fire, so a route that never reaches the tab shows up here as a zoom
/// that did not change or a page count that did not grow. The append is followed all the way to
/// the file: the document is re-opened from disk at the end, which is what a second reader sees.
pub(super) fn bench_pdf(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let app = app.clone();
    // The pages are measured on the render thread, so nothing about the layout is known until it
    // has reported back.
    glib::timeout_add_local_once(Duration::from_millis(600), move || {
        let Some(pdf) = app.active_pdf() else {
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
        // Longer than the tab's own save timer, so what is printed is the file and not the plan.
        glib::timeout_add_local_once(Duration::from_millis(1400), move || {
            let Some(pdf) = app.active_pdf() else {
                println!("bench pdf no_tab");
                return bench_quit(&app);
            };
            let sizes = accent_core::pdf::PdfDoc::open(pdf.path())
                .and_then(|doc| doc.page_sizes())
                .unwrap_or_default();
            println!(
                "bench pdf added pages={} at={} on_disk={:?}",
                pdf.page_count(),
                pdf.place().page,
                sizes
            );
            bench_pdf_renamed(&app);
        });
    });
}

/// The same document under a new name: rename it the way a dropped row does, then append another
/// page and read the file back.
///
/// The render thread owns the path it reloads from and saves to, so a rename it was never told
/// about shows up here as a page count on disk that did not grow — the save going to a name that
/// is no longer there.
fn bench_pdf_renamed(app: &Rc<App>) {
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
    let app = app.clone();
    // The rename runs on a worker and the watcher's event lands a turn after it.
    glib::timeout_add_local_once(Duration::from_millis(1500), move || {
        let Some(pdf) = app.active_pdf() else {
            println!("bench pdf no_tab");
            return bench_quit(&app);
        };
        println!(
            "bench pdf renamed key={:?} reads={:?} old_gone={}",
            pdf.key(),
            pdf.path().file_name().map(|n| n.to_string_lossy()),
            !was.exists()
        );
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-add-page", None);
        glib::timeout_add_local_once(Duration::from_millis(1400), move || {
            let sizes = accent_core::pdf::PdfDoc::open(pdf.path())
                .and_then(|doc| doc.page_sizes())
                .unwrap_or_default();
            println!(
                "bench pdf renamed_added pages={} on_disk={}",
                pdf.page_count(),
                sizes.len()
            );
            bench_quit(&app);
        });
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
