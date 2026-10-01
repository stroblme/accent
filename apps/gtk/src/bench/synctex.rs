//! SyncTeX both ways through the window's own commands (`ACCENT_BENCH_PDF=synctex:<rel_pdf>`).

use super::*;

/// Points of an article's first two pages, as a page from 0 and PDF points from its top-left
/// corner: the first section's heading, the first and third lines of its first paragraph, a
/// line further down, the second page's first line of text, and the margin left of its second.
const POINTS: [(usize, f32, f32); 6] = [
    (0, 150.0, 130.0),
    (0, 300.0, 157.0),
    (0, 200.0, 181.0),
    (0, 180.0, 249.0),
    (1, 150.0, 155.0),
    (1, 40.0, 168.0),
];

/// Open the PDF, say whether Go to Source and Show in PDF are offered, then for each of
/// [`POINTS`] go to the source as the page's menu does and print where the caret landed (or the
/// toast), and from that line show it in the PDF again, printing the line of text marked and
/// whether it holds the point. Point it at a LaTeX build made with `-synctex=1` in a scratch
/// vault; a source `\input` from outside the vault prints its toast.
pub(super) fn bench_synctex(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let Some(pdf) = super::pdf::opened(&app, &rel).await else {
            println!("bench synctex no_tab");
            return bench_quit(&app);
        };
        let menu = pdf.selection_menu(10.0, 10.0);
        let items = menu.menu_model().map(|m| fileops::labels(&m));
        menu.popdown();
        println!("bench synctex pdf {} menu={items:?}", offered(&app));
        for (page, x, y) in POINTS {
            app.open_path(&rel);
            let Some(pdf) = until(|| app.active_pdf()).await else {
                println!("bench synctex no_pdf");
                break;
            };
            let toasted = app.toasted.get();
            pdf.point_at(page, x, y);
            let _ = WidgetExt::activate_action(&app.window, "win.pdf-go-to-source", None);
            let landed = until(|| match app.toasted.get() == toasted {
                true => app.active().map(Some),
                false => Some(None),
            });
            let at = format!("page={page} x={x} y={y}");
            let Some(Some(tab)) = landed.await else {
                // Queued behind the vault's "Indexed …" toast while that is up.
                let said = until(|| bench_said(&app)).await;
                println!("bench synctex edit {at} said={said:?}");
                continue;
            };
            let line = tab.cursor_line();
            println!(
                "bench synctex edit {at} -> {}:{line} {}",
                tab.rel(),
                offered(&app)
            );
            // Back from that line to the PDF: the same page, and a line of text near the point.
            let _ = WidgetExt::activate_action(&app.window, "win.show-in-pdf", None);
            let marked = until(|| app.active_pdf().and_then(|pdf| pdf.marked())).await;
            let Some((back, rect)) = marked else {
                println!(
                    "bench synctex view {}:{line} said={:?}",
                    tab.rel(),
                    bench_said(&app)
                );
                continue;
            };
            println!(
                "bench synctex view {}:{line} -> page={back} top={:.1} bottom={:.1} near={}",
                tab.rel(),
                rect.top,
                rect.bottom,
                back == page && (rect.top - 12.0..=rect.bottom + 12.0).contains(&y),
            );
        }
        // Show in PDF with the PDF closed: it opens again, and the mark waits for its pages.
        if let Some(pdf) = app.doc_for(&rel).and_then(|doc| doc.pdf().cloned()) {
            app.close_page(&pdf.page);
        }
        let tex = until(|| app.active().filter(|_| app.doc_for(&rel).is_none())).await;
        let _ = WidgetExt::activate_action(&app.window, "win.show-in-pdf", None);
        let marked = until(|| app.active_pdf().and_then(|pdf| pdf.marked())).await;
        println!(
            "bench synctex reopened from={:?} marked={marked:?} {:?}",
            tex.map(|tab| format!("{}:{}", tab.rel(), tab.cursor_line())),
            app.active_pdf().map(|pdf| pdf.geometry())
        );
        bench_quit(&app);
    });
}

/// Whether the two commands are enabled as the window stands.
fn offered(app: &Rc<App>) -> String {
    let on = |name| {
        app.window
            .lookup_action(name)
            .is_some_and(|a| a.is_enabled())
    };
    format!(
        "go_to_source={} show_in_pdf={}",
        on("pdf-go-to-source"),
        on("show-in-pdf")
    )
}

/// `found` once it answers, polled for twelve seconds: a toast waits for the one before it.
async fn until<T>(found: impl Fn() -> Option<T>) -> Option<T> {
    for _ in 0..240 {
        if let Some(it) = found() {
            return Some(it);
        }
        glib::timeout_future(Duration::from_millis(50)).await;
    }
    None
}
