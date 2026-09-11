//! Drills over a PDF: the layout either side of Fit Height.

use super::*;

/// Open a PDF, leave the reader halfway down its second page, and fit the page from there.
///
/// Fit Height is fired as the window action the status bar's menu and the palette both fire, so a
/// route that never reaches the tab shows up here as a zoom that did not change.
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
        bench_quit(&app);
    });
}
