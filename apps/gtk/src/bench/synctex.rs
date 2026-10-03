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
/// vault; a source `\input` from outside the vault prints its toast. A build without a SyncTeX
/// file prints what the palette and the `.tex` beside it say instead ([`without_synctex`]).
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
        if app.synctex_missing(app.active_doc().as_ref()).0 {
            without_synctex(&app, &rel).await;
            return bench_quit(&app);
        }
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
        accent_writes(&app, &rel).await;
        bench_quit(&app);
    });
}

/// accent's own writes into the PDF: a stroke leaves the build trusted, through the tab closed and
/// opened again, and a page added takes it away, an Undo of it too, each printed as the window
/// offers Go to Source once it has looked again, as a tab switch makes it.
async fn accent_writes(app: &Rc<App>, rel: &str) {
    let saved = || glib::timeout_future(Duration::from_millis(1500));
    let offer = |step: &str| {
        app.sync_synctex(app.active_doc());
        println!("bench synctex {step} {}", offered(app));
    };
    let Some(pdf) = app.active_pdf() else {
        return println!("bench synctex no_pdf");
    };
    pdf.draw(0, vec![(60.0, 60.0), (90.0, 80.0)]);
    saved().await;
    offer("inked");
    app.close_page(&pdf.page);
    let Some(pdf) = super::pdf::opened(app, rel).await else {
        return println!("bench synctex no_pdf");
    };
    offer("inked_reopened");
    pdf.edit_pages(accent_core::pdf::PageEdit::Insert(pdf.page_count()));
    // Before the save lands, which is when the page is in the file.
    glib::timeout_future(Duration::from_millis(300)).await;
    offer("added");
    saved().await;
    offer("added_saved");
    let _ = WidgetExt::activate_action(&app.window, "win.pdf-undo", None);
    saved().await;
    offer("added_undone");
}

/// A LaTeX build without a SyncTeX file: the palette's rows for the two commands over the PDF and
/// over the `.tex` of its name beside it, then that tab's menu each second for eight, for a
/// secondary press on it through XTEST to change.
async fn without_synctex(app: &Rc<App>, rel: &str) {
    println!(
        "bench synctex no_synctex pdf palette={:?}",
        palette_rows(app).await
    );
    let tex = format!("{}.tex", rel.trim_end_matches(".pdf"));
    app.open_path(&tex);
    let Some(tab) = until(|| app.active().filter(|tab| tab.rel() == tex)).await else {
        return println!("bench synctex no_tex {tex}");
    };
    println!(
        "bench synctex no_synctex tex palette={:?}",
        palette_rows(app).await
    );
    for _ in 0..8 {
        let menu = tab.view.extra_menu().map(|m| fileops::labels(&m));
        println!("bench synctex no_synctex tex menu={menu:?}");
        glib::timeout_future(Duration::from_secs(1)).await;
    }
}

/// What the command palette's rows for Go to Source and Show in PDF say under their names, and
/// whether each takes a pick.
async fn palette_rows(app: &Rc<App>) -> Vec<(String, String, bool)> {
    let mut rows = Vec::new();
    for label in ["Go to Source", "Show in PDF"] {
        let _ = WidgetExt::activate_action(&app.window, "win.palette-commands", None);
        glib::timeout_future(Duration::from_millis(400)).await;
        let Some(dialog) = app.window.visible_dialog() else {
            continue;
        };
        let entry = find_widget(dialog.upcast_ref(), &|w| w.is::<gtk::SearchEntry>());
        if let Some(entry) = entry.and_downcast::<gtk::SearchEntry>() {
            entry.set_text(&format!(">{label}"));
        }
        glib::timeout_future(Duration::from_millis(400)).await;
        let named = |w: &gtk::Widget| {
            w.downcast_ref::<gtk::Label>()
                .is_some_and(|l| l.label() == label)
        };
        if let Some(name) = find_widget(dialog.upcast_ref(), &named) {
            let under = name.next_sibling().and_downcast::<gtk::Label>();
            let says = under.map(|l| l.label().to_string()).unwrap_or_default();
            let picks = name.parent().is_some_and(|row| row.is_sensitive());
            rows.push((label.to_string(), says, picks));
        }
        dialog.force_close();
        glib::timeout_future(Duration::from_millis(200)).await;
    }
    rows
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
