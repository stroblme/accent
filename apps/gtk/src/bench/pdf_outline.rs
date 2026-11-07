//! PDF drills for the Outline pane's bookmarks and comments.

use super::pdf::{labels, online, opened, render_threads, settled, written};
use super::*;

/// The Outline pane following the reader: the bookmark the page is under as `rel` is scrolled
/// through (a scroll, not a jump), then the list after a page edit, which must be the same list
/// refilled; with `,<rel_diagram>` a diagram's pages as it turns to each. Every line says which
/// row is selected, whether it is on screen and who has the keyboard, which a follow never takes.
pub(super) fn bench_pdf_bookmarks(app: &Rc<App>, arg: &str) {
    let (rel, diagram) = match arg.split_once(',') {
        Some((rel, diagram)) => (rel.to_string(), Some(diagram.to_string())),
        None => (arg.to_string(), None),
    };
    let app = app.clone();
    glib::spawn_future_local(async move {
        let Some(pdf) = opened(&app, &rel).await else {
            println!("bench bookmarks no_tab");
            return bench_quit(&app);
        };
        // Showing the pane hands it the keyboard; the reader takes it back, as a click would.
        app.show_pane("outline");
        pdf.key_target().grab_focus();
        glib::timeout_future(Duration::from_millis(300)).await;
        let list = outline_view(&app);
        for page in [0, 1, 2, 3, 4, 2, 0] {
            pdf.scroll_to(pdfview::Anchor {
                page,
                u: 0.0,
                v: 0.5,
            });
            glib::timeout_future(Duration::from_millis(300)).await;
            println!(
                "bench bookmarks page={} {}",
                pdf.current_page() + 1,
                outline_row(&app)
            );
        }
        // The first page goes to the end, and the reader, on it, with it.
        pdf.edit_pages(accent_core::pdf::PageEdit::Move { from: 0, to: 4 });
        written(&app).await;
        println!(
            "bench bookmarks moved page={} kept={} {}",
            pdf.current_page() + 1,
            list.is_some() && outline_view(&app) == list,
            outline_row(&app)
        );
        if let Some(rel) = diagram {
            app.open_path(&rel);
            glib::timeout_future(Duration::from_millis(800)).await;
            let Some(d) = app.active_diagram() else {
                println!("bench bookmarks no_diagram");
                return bench_quit(&app);
            };
            d.key_target().grab_focus();
            for page in (0..d.page_count()).rev() {
                d.show_page(page);
                glib::timeout_future(Duration::from_millis(300)).await;
                println!(
                    "bench bookmarks diagram_page={} {}",
                    page + 1,
                    outline_row(&app)
                );
            }
        }
        bench_quit(&app);
    });
}

/// The Outline pane's list, if it is showing one.
fn outline_view(app: &Rc<App>) -> Option<gtk::ListView> {
    app.sidebar
        .get()
        .and_then(|s| s.outline_child())
        .and_then(|c| find_widget(&c, &|w| w.is::<gtk::ListView>()))
        .and_downcast::<gtk::ListView>()
}

/// The row the Outline pane has selected, its text, whether it is on screen, and who has the
/// keyboard.
fn outline_row(app: &Rc<App>) -> String {
    let Some(list) = outline_view(app) else {
        return "list=none".to_string();
    };
    let Some(selection) = list.model().and_downcast::<gtk::SingleSelection>() else {
        return "selection=none".to_string();
    };
    let text = selection
        .selected_item()
        .and_downcast::<gtk::StringObject>()
        .map(|s| s.string().to_string());
    // Every row is one line of the same label, so a row's place is its index times the height.
    let adj = list.vadjustment().expect("bench adjustment");
    let height = adj.upper() / f64::from(selection.n_items().max(1));
    let top = f64::from(selection.selected()) * height;
    let in_view = text.is_some()
        && top >= adj.value() - 0.5
        && top + height <= adj.value() + adj.page_size() + 0.5;
    let focus = gtk::prelude::GtkWindowExt::focus(&app.window)
        .map_or("none".to_string(), |w| w.type_().name().to_string());
    format!("selected={text:?} in_view={in_view} focus={focus}")
}

/// Inspect generated `Attachments/comments.pdf` with a note highlight over its first word.
/// Report tooltips, pinned popovers and suppression over annotations, non-comment marks and
/// empty space. Tools must not open comments; an unarmed click on the note highlight opens its
/// note.
///
/// Repeat after Export Highlights, then activate the third-page comment from the Outline list
/// and report page and popover.
pub(super) fn bench_pdf_comments(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        online(&app).await;
        let note = "Comment links.md".to_string();
        if let Some(vault) = app.vault().cloned() {
            let text = format!("[[{rel}#page=1&selection=0,0,0,3|Ada]]\n");
            if let Err(e) = vault.save(&note, &text, None) {
                println!("bench comments note_not_written {e:?}");
            }
            let linked = until("comments", "backlink", 10_000, || {
                let links = vault.backlinks(&rel).ok()?;
                links.iter().any(|b| b.src_rel_path == note).then_some(())
            });
            println!("bench comments linked={}", linked.await.is_some());
        }
        let Some(pdf) = opened(&app, &rel).await else {
            println!("bench comments no_tab");
            return bench_quit(&app);
        };
        let Ok(view) = pdf.views()[0].clone().downcast::<pdfview::PdfView>() else {
            return bench_quit(&app);
        };
        // A point of the page, in its own points from the top left, in the view's coordinates.
        let at = |(x, y): (f32, f32)| {
            let r = view.widget_rect(0, &accent_core::pdf::Rect::from_corners((x, y), (x, y)))?;
            Some((f64::from(r.x()), f64::from(r.y())))
        };
        // "Ada", where the note's highlight lands.
        let ada = at((52.0, 46.0));
        let landed = until("comments", "highlight", 10_000, || {
            let (x, y) = ada?;
            view.highlight_at(x, y)?;
            pdf.page_comments(0)
        });
        let comments = landed.await.unwrap_or_default();
        println!("bench comments page=0 count={}", comments.len());
        let probe = |name: &str, point: Option<(f64, f64)>| {
            let Some((x, y)) = point else {
                return println!("bench comments {name} off_screen");
            };
            let shows = pdf.hover_at(x, y).map(|tip| labels(&tip));
            let pinned = pdf.click_at(x, y);
            let pins = pinned.as_ref().map(|p| labels(p.upcast_ref()));
            let tip_after = pdf.hover_at(x, y).is_some();
            let up = pinned.as_ref().is_some_and(|p| p.is_visible());
            if let Some(pinned) = pinned {
                pinned.popdown();
            }
            println!(
                "bench comments {name} shows={shows:?} pins={pins:?} up={up} tip_after={tip_after}"
            );
        };
        for c in &comments {
            let a = c.areas[0];
            let middle = ((a.left + a.right) / 2.0, (a.top + a.bottom) / 2.0);
            probe(c.author.as_deref().unwrap_or("nameless"), at(middle));
        }
        // Where gen-vault's `comments_pdf` puts them.
        for (name, point) in [
            ("struck", (107.0, 105.0)),
            ("typed", (140.0, 205.0)),
            ("hidden", (350.0, 90.0)),
            ("link", (234.0, 75.0)),
            ("empty", (300.0, 270.0)),
        ] {
            probe(name, at(point));
        }
        pdf.set_mode(pdfview::Mode::Pen);
        probe("pen", at((116.0, 45.0)));
        // The Eraser's click on the note's highlight is an erase, and opens nothing.
        pdf.set_mode(pdfview::Mode::Eraser);
        probe("eraser", ada);
        glib::timeout_future(Duration::from_millis(500)).await;
        println!(
            "bench comments eraser note_opened={}",
            app.tab_for(&note).is_some()
        );
        pdf.set_mode(pdfview::Mode::Read);
        probe("note", ada);
        let opened = until("comments", "note", 10_000, || app.tab_for(&note))
            .await
            .is_some();
        println!("bench comments note_opened={opened}");
        // Export Highlights writes the note's quote into the file: a comment from then on.
        app.open_path(&rel);
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-export-highlights", None);
        let more = until("comments", "exported", 10_000, || {
            pdf.page_comments(0)
                .filter(|now| now.len() > comments.len())
        });
        let count = more.await.map(|now| now.len());
        let shows = ada
            .and_then(|(x, y)| pdf.hover_at(x, y))
            .map(|tip| labels(&tip));
        println!("bench comments exported count={count:?} note shows={shows:?}");
        // The Outline pane lists every page's comments; a row goes to its comment and pins it, as
        // a click on it does.
        app.show_pane("outline");
        let Some(sidebar) = app.sidebar.get() else {
            return bench_quit(&app);
        };
        let lines = until("comments", "outline", 10_000, || {
            let lines = sidebar.outline_lines(None);
            let read = lines.iter().any(|l| l.contains("Second thoughts"));
            read.then_some(lines)
        });
        let lines = lines.await.unwrap_or_default();
        for line in &lines {
            println!("bench comments listed {line}");
        }
        sidebar.outline_lines(lines.iter().position(|l| l.contains("Second thoughts")));
        let pinned = pdf.comments_read().1;
        let pins = pinned.as_ref().map(|p| labels(p.upcast_ref()));
        let up = pinned.is_some_and(|p| p.is_visible());
        let page = pdf.current_page() + 1;
        println!("bench comments row page={page} pins={pins:?} up={up}");
        bench_quit(&app);
    });
}

/// The Outline pane's comments read off `rel`, a long document: when the first screen had painted
/// and how many pages had been read by then, the same after a jump to the middle of it, and when
/// every page had been read; then the document opened again and closed at once, and how long its
/// thread outlived the tab, which ends the walk.
pub(super) fn bench_pdf_walk(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let t0 = Instant::now();
        let Some(pdf) = opened(&app, &rel).await else {
            println!("bench walk no_tab");
            return bench_quit(&app);
        };
        let pages = pdf.page_count();
        println!("bench walk pages={pages} opened_ms={}", ms_since(t0) as u64);
        settled(&pdf, "walk first_screen").await;
        println!("bench walk first_screen read={}", pdf.comments_read().0);
        pdf.goto_page(pages / 2);
        settled(&pdf, "walk middle").await;
        println!("bench walk middle read={}", pdf.comments_read().0);
        while pdf.comments_read().0 < pages && t0.elapsed() < Duration::from_secs(60) {
            glib::timeout_future(Duration::from_millis(20)).await;
        }
        println!(
            "bench walk read={} all_ms={} rows={}",
            pdf.comments_read().0,
            ms_since(t0) as u64,
            pdf.comment_rows().len()
        );
        app.close_page(&pdf.page);
        drop(pdf);
        glib::timeout_future(Duration::from_millis(300)).await;
        let Some(pdf) = opened(&app, &rel).await else {
            return bench_quit(&app);
        };
        let at_close = pdf.comments_read().0;
        app.close_page(&pdf.page);
        drop(pdf);
        let t1 = Instant::now();
        while render_threads() > 0 && t1.elapsed() < Duration::from_secs(10) {
            glib::timeout_future(Duration::from_millis(5)).await;
        }
        println!(
            "bench walk closed read={at_close}/{pages} threads={} gone_ms={}",
            render_threads(),
            ms_since(t1) as u64
        );
        bench_quit(&app);
    });
}
