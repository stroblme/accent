//! Drills over a PDF: the layout either side of Fit Height, the page Add Page After puts in, the
//! page edits and their menus, the Outline pane following the reader, and the blank document New
//! Drawing writes.

use super::*;

/// Open a PDF, leave the reader halfway down its second page, fit the page from there, and then
/// add one after it.
///
/// Fit Height and Add Page After are fired as the window actions the status bar's menus, the
/// page's own menu and the palette all fire, so a route that never reaches the tab shows up here
/// as a zoom that did not change or a page count that did not grow. The new page is followed all
/// the way to the file: the document is re-opened from disk at the end, which is what a second
/// reader sees.
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
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-add-page-after", None);
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
/// thumbnail strip makes, a page added before the one being read, one added after the last page
/// and the first page deleted through the window actions — at once, no dialog being asked —
/// each followed to the file. Then Undo walks all four back through the window's action, newest
/// first, and Redo makes them again, the file read after each. A note linking into pages 1 to 3
/// (a highlight, a jump, a markdown link, an HTML `href` and a reference definition) is written
/// and opened in a tab first, and after every step its links' pages are printed as the file and
/// the tab's buffer hold them, with what the toast said, if it said anything; it is renamed
/// between the delete and its Undo, which finds the links the delete left by the new name. Then
/// Delete Page and Add Page Before from the page's menu opened on the third page while the first
/// is read, each undone, with the page being read and the pages after each. Then the items of
/// the two
/// menus that offer them: the page's own, without a selection and with one, and the status bar's
/// page count, opened as a click does. What a headless run cannot reach is the pointer's half: the
/// drag itself, the buttons on hover, the drop bar and the scroll at the strip's edge.
pub(super) fn bench_pdf_pages(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let note = RefCell::new(linked_note(&app, &rel).await);
        let Some(pdf) = opened(&app, &rel).await else {
            println!("bench pages no_tab");
            return bench_quit(&app);
        };
        let said = Cell::new(app.toasted.get());
        let links = || links_read(&app, &note.borrow(), &said);
        println!("bench pages opened {} {}", pages_read(&pdf), links());
        pdf.edit_pages(accent_core::pdf::PageEdit::Move { from: 0, to: 2 });
        written(&app).await;
        println!("bench pages moved {} {}", pages_read(&pdf), links());
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-add-page-before", None);
        written(&app).await;
        println!("bench pages before {} {}", pages_read(&pdf), links());
        // After the last page, which is how a document grows at its end.
        pdf.goto_page(pdf.page_count() - 1);
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-add-page-after", None);
        written(&app).await;
        println!("bench pages after {} {}", pages_read(&pdf), links());
        // A page with text on it, so Undo is seen putting the text back.
        pdf.goto_page(0);
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-delete-page", None);
        written(&app).await;
        println!(
            "bench pages deleted dialog={} {} {}",
            app.window.visible_dialog().is_some(),
            pages_read(&pdf),
            links()
        );
        if let Some(ops) = app.ops().cloned() {
            let to = "Page links renamed.md".to_string();
            crate::fileops::move_all(&ops, vec![(note.borrow().clone(), to.clone())]);
            written(&app).await;
            note.replace(to);
            println!("bench pages renamed {}", links());
        }
        let walks = [
            (
                "undo",
                "win.pdf-undo",
                ["deleted", "after", "before", "moved"],
            ),
            (
                "redo",
                "win.pdf-redo",
                ["moved", "before", "after", "deleted"],
            ),
        ];
        for (walk, action, steps) in walks {
            for step in steps {
                let _ = WidgetExt::activate_action(&app.window, action, None);
                written(&app).await;
                println!("bench pages {walk}_{step} {} {}", pages_read(&pdf), links());
            }
        }
        println!("bench pages history={:?}", pdf.history());
        // The page's menu acts on the page it was opened on, the third here while the first is
        // being read, and Undo puts that page back where it was.
        for (step, action) in [
            ("menu_deleted", "win.pdf-delete-page"),
            ("menu_before", "win.pdf-add-page-before"),
        ] {
            pdf.goto_page(0);
            pdf.point_at(2, 100.0, 100.0);
            let _ = WidgetExt::activate_action(&app.window, action, None);
            written(&app).await;
            println!("bench pages {step} {}", pages_read(&pdf));
            let _ = WidgetExt::activate_action(&app.window, "win.pdf-undo", None);
            written(&app).await;
            println!("bench pages {step}_undone {}", pages_read(&pdf));
        }
        let items = |menu: gtk::PopoverMenu| {
            let items = menu.menu_model().map(|m| fileops::labels(&m));
            menu.popdown();
            items
        };
        println!(
            "bench pages menu {:?}",
            items(pdf.selection_menu(10.0, 10.0))
        );
        // "Page" on the first page, selected as a followed link selects it.
        pdf.show_link(0, Some([0, 0, 0, 4]));
        glib::timeout_future(Duration::from_millis(500)).await;
        println!(
            "bench pages menu_selected {:?}",
            items(pdf.selection_menu(10.0, 10.0))
        );
        let count = app.statusbar.facts_control();
        count.emit_clicked();
        let popover = find_widget(count.upcast_ref(), &|w| w.is::<gtk::PopoverMenu>());
        println!(
            "bench pages count_menu tooltip={:?} clickable={} {:?}",
            count.tooltip_text(),
            count.can_target(),
            popover.and_downcast::<gtk::PopoverMenu>().and_then(items)
        );
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

/// Every page renders however the reader gets to it. With the Outline pane up, both views of
/// `rel` are scrolled for two seconds from each tenth of the document — past each other, or the
/// reading view left where it jumped while the strip is browsed twenty pages on — then zoomed in
/// four steps and out four; after each burst both views must come to paint everything they want.
/// Each burst prints how long that took, or, when what is missing has not changed in five
/// seconds, what it is.
pub(super) fn bench_pdf_render(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let Some(pdf) = opened(&app, &rel).await else {
            println!("bench pdf render no_tab");
            return bench_quit(&app);
        };
        app.show_pane("outline");
        let pages = pdf.page_count();
        let mut stuck = 0;
        for burst in 0..10 {
            let start = (burst * pages / 10) as f32;
            for step in 0..120 {
                let t = step as f32 / 20.0;
                // Even bursts scroll the two past each other; odd ones leave the reading view on
                // the page it jumped to while the strip is browsed twenty pages on.
                match burst % 2 {
                    0 => pdf.scroll_both(start + t, start + 6.0 - t),
                    _ => pdf.scroll_both(start, start + 20.0 + t),
                }
                glib::timeout_future(Duration::from_millis(16)).await;
            }
            let what = format!("burst={burst} page={}", pdf.current_page() + 1);
            stuck += usize::from(settled(&pdf, &what).await);
        }
        for out in [false, true] {
            for _ in 0..4 {
                pdf.zoom_step(out);
                glib::timeout_future(Duration::from_millis(60)).await;
            }
            let what = format!("zoom_out={out} {:?}", pdf.zoom_label());
            stuck += usize::from(settled(&pdf, &what).await);
        }
        println!("bench pdf render pages={pages} stuck={stuck}");
        bench_quit(&app);
    });
}

/// A document zoomed deep. `arg` is `<rel>[,<percent>]`, 800 % unless given: the break between
/// the first two pages across the middle of the view, left alone for 20 s, each second printing
/// how many tiles the view wants and has not got, how many on screen it shows other than sharp,
/// how many landed in that second and how many of those had landed before — the render thread
/// never idle and the set churning, where the cache cannot hold what the view asks for. Then a
/// reader at that zoom: a wheel notch, a tenth of the view, every 100 ms for 10 s, and ten Page
/// Downs of a whole view 600 ms apart, each counting the steps after which a tile on screen was
/// not yet sharp.
pub(super) fn bench_pdf_deep(app: &Rc<App>, arg: &str) {
    let (rel, percent) = match arg.split_once(',') {
        Some((rel, percent)) => (rel.to_string(), percent.parse().unwrap_or(800.0)),
        None => (arg.to_string(), 800.0),
    };
    let app = app.clone();
    glib::spawn_future_local(async move {
        let Some(pdf) = opened(&app, &rel).await else {
            println!("bench pdf deep no_tab");
            return bench_quit(&app);
        };
        pdf.set_zoom(pdfview::PdfZoom::Scale(percent / 100.0));
        glib::timeout_future(Duration::from_millis(100)).await;
        pdf.scroll_both(1.0, 0.0);
        pdf.scroll_by(-0.5);
        let sf = pdf.views()[0].scale_factor();
        println!("bench pdf deep sf={sf} {}", pdf.geometry());
        let (t0, mut last) = (Instant::now(), None);
        let mut seen = HashSet::new();
        for second in 1..=20 {
            let (mut landed, mut again) = (0, 0);
            for _ in 0..10 {
                glib::timeout_future(Duration::from_millis(100)).await;
                let (_, _, rendered) = pdf.tiles();
                if !rendered.is_empty() {
                    last = Some(ms_since(t0) as u64);
                }
                landed += rendered.len();
                again += rendered
                    .into_iter()
                    .filter(|key| !seen.insert(*key))
                    .count();
            }
            let (missing, unsharp, _) = pdf.tiles();
            println!(
                "bench pdf deep t={second} missing={missing} unsharp={unsharp} \
                 landed={landed} again={again}"
            );
        }
        println!("bench pdf deep last_tile_ms={last:?}");
        for (name, step, every, steps) in [("wheel", 0.1, 100, 100), ("page_down", 1.0, 600, 10)] {
            let (mut blurred, mut worst) = (0, 0);
            for _ in 0..steps {
                pdf.scroll_by(step);
                // A few frames after the step: what the reader sees on arriving.
                glib::timeout_future(Duration::from_millis(50)).await;
                let (_, unsharp, _) = pdf.tiles();
                blurred += usize::from(unsharp > 0);
                worst = worst.max(unsharp);
                glib::timeout_future(Duration::from_millis(every - 50)).await;
            }
            println!("bench pdf deep {name} steps={steps} unsharp_steps={blurred} worst={worst}");
        }
        bench_quit(&app);
    });
}

/// A PDF that will not open waits for its file and opens once the file is whole. `arg`, relative
/// to the vault or an absolute path outside it (which no watcher reports on), is cut in half in
/// place and opened; then written back in six pieces a quarter of a second apart, the way a LaTeX
/// run writes; then, open, left for a page five on, cut in half under the reader and mended at
/// once. Each step prints the waiting page's words or the pages and the page being read, how long
/// after the last write, and how many times the file has been opened or failed to: the six pieces
/// must cost one open, not one each. It writes into `arg`, so point it at a scratch copy of a
/// document of a dozen pages or more.
pub(super) fn bench_pdf_broken(app: &Rc<App>, arg: &str) {
    let (app, arg) = (app.clone(), arg.to_string());
    glib::spawn_future_local(async move {
        let file = match Path::new(&arg).is_absolute() {
            true => PathBuf::from(&arg),
            false => app.root().join(&arg),
        };
        let Ok(whole) = std::fs::read(&file) else {
            println!("bench pdf broken unreadable");
            return bench_quit(&app);
        };
        let half = &whole[..whole.len() / 2];
        let say = |pdf: &pdftab::PdfTab, step: &str, since: Instant| {
            let (waiting, opens) = pdf.waiting();
            println!(
                "bench pdf broken {step} waiting={waiting:?} pages={} page={} opens={opens} ms={}",
                pdf.page_count(),
                pdf.place().page + 1,
                ms_since(since) as u64
            );
        };
        let _ = std::fs::write(&file, half);
        let t0 = Instant::now();
        app.open_path(&arg);
        let failed = || app.active_pdf().filter(|pdf| pdf.waiting().0.is_some());
        let Some(pdf) = until(failed).await else {
            println!("bench pdf broken never_waited");
            return bench_quit(&app);
        };
        say(&pdf, "opened", t0);
        // Truncated where it lies and written a piece at a time, the file staying half-written
        // for longer than the tab waits for it to settle.
        if let Ok(mut out) = std::fs::File::create(&file) {
            for piece in whole.chunks(whole.len().div_ceil(6)) {
                let _ = std::io::Write::write_all(&mut out, piece);
                glib::timeout_future(Duration::from_millis(250)).await;
            }
        }
        let written = Instant::now();
        until(|| (pdf.page_count() > 0).then_some(())).await;
        say(&pdf, "written", written);
        pdf.goto_page(pdf.page_count() / 2);
        glib::timeout_future(Duration::from_secs(1)).await;
        let _ = std::fs::write(&file, half);
        let broken = Instant::now();
        // A reader still reading, whose next page is what finds the file changed under it where
        // no watcher says so.
        pdf.goto_page(pdf.current_page() + 5);
        until(|| pdf.waiting().0.map(drop)).await;
        say(&pdf, "broken", broken);
        let _ = std::fs::write(&file, &whole);
        let mended = Instant::now();
        until(|| (pdf.page_count() > 0).then_some(())).await;
        say(&pdf, "mended", mended);
        bench_quit(&app);
    });
}

/// What `ready` hands back, once it does, checked every 50 ms for ten seconds.
async fn until<T>(ready: impl Fn() -> Option<T>) -> Option<T> {
    for _ in 0..200 {
        if let Some(found) = ready() {
            return Some(found);
        }
        glib::timeout_future(Duration::from_millis(50)).await;
    }
    None
}

/// Wait for both views of `pdf` to have painted everything they want and print how long that
/// took, or, once what is missing has not changed in five seconds, print it: true when it did not.
async fn settled(pdf: &Rc<pdftab::PdfTab>, what: &str) -> bool {
    let (t0, mut since) = (Instant::now(), Instant::now());
    let mut last = pdf.unrendered();
    loop {
        glib::timeout_future(Duration::from_millis(100)).await;
        let now = pdf.unrendered();
        if now.0.is_empty() && now.1.is_empty() {
            println!("bench pdf render {what} settled_ms={}", ms_since(t0) as u64);
            return false;
        }
        if now != last {
            (last, since) = (now, Instant::now());
        } else if since.elapsed() > Duration::from_secs(5) {
            println!(
                "bench pdf render {what} stuck view={:?} strip={:?}",
                now.0, now.1
            );
            return true;
        }
    }
}

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

/// Write `Page links.md` at the vault root, linking into pages 1 to 3 of `pdf` as a highlight,
/// a jump, a markdown link, an HTML `href` into page 2 and a reference definition into page 1,
/// wait until the index has it as one of the PDF's backlinks, and open
/// it in a tab, which a rewrite has to reload.
async fn linked_note(app: &Rc<App>, pdf: &str) -> String {
    let note = "Page links.md".to_string();
    let encoded = accent_core::markdown::percent_encode(pdf);
    let text = format!(
        "[[{pdf}#page=1&selection=0,0,0,4|Page]] [[{pdf}#page=2]] [three]({encoded}#page=3)\n\
         <a href=\"{encoded}#page=2\">two</a> [one][d]\n\n[d]: {encoded}#page=1\n"
    );
    let Some(vault) = app.vault().cloned() else {
        return note;
    };
    if let Err(e) = vault.save(&note, &text, None) {
        println!("bench pages note_not_written {e:?}");
    }
    for _ in 0..100 {
        let linked = vault
            .backlinks(pdf)
            .is_ok_and(|links| links.iter().any(|b| b.src_rel_path == note));
        if linked {
            break;
        }
        glib::timeout_future(Duration::from_millis(100)).await;
    }
    app.open_path(&note);
    for _ in 0..50 {
        if app.tab_for(&note).is_some() {
            break;
        }
        glib::timeout_future(Duration::from_millis(100)).await;
    }
    note
}

/// The pages the note's links name, in the order it holds them — on disk, and in its tab unless
/// the tab holds the same — and what the toast said since the last time this was asked: how the
/// notes followed the step before it.
fn links_read(app: &Rc<App>, note: &str, said: &Cell<usize>) -> String {
    let pages = |text: &str| -> Vec<String> {
        text.match_indices("#page=")
            .map(|(at, found)| {
                let rest = &text[at + found.len()..];
                let end = rest.find(|c: char| !c.is_ascii_digit());
                rest[..end.unwrap_or(rest.len())].to_string()
            })
            .collect()
    };
    let on_disk = app
        .vault()
        .and_then(|v| v.read(note).ok())
        .map(|(text, _)| pages(&text))
        .unwrap_or_default();
    let tab = match app.tab_for(note).map(|tab| pages(&tab.text())) {
        Some(held) if held == on_disk => "same".to_string(),
        held => format!("{held:?}"),
    };
    let now = app.toasted.get();
    let toast = (now > said.replace(now))
        .then(|| app.toasts.shown().into_iter().next())
        .flatten();
    format!("links={on_disk:?} tab={tab} said={toast:?}")
}

/// Open `rel` and hand back the tab once its pages are known.
///
/// Both halves wait: a remote window is up and taking commands well before its host has answered,
/// and the pages are measured on the render thread after a fetch that takes as long as the link
/// does. A local vault passes straight through both.
pub(super) async fn opened(app: &Rc<App>, rel: &str) -> Option<Rc<pdftab::PdfTab>> {
    online(app).await;
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

/// The same document under a new name: rename it the way a dropped row does, then add another
/// page and read the file back, then write a one-page document over it from outside.
///
/// The render thread owns the path it reloads from and saves to, so a rename it was never told
/// about shows up here as a page count on disk that did not grow — the save going to a name that
/// is no longer there. On a remote vault the cached copy has to move with the file, or the page
/// goes up beside it as `(edited)` and the reader keeps a copy nothing refreshes.
async fn bench_pdf_renamed(app: &Rc<App>) {
    let (Some(pdf), Some(ops), Some(vault)) =
        (app.active_pdf(), app.ops().cloned(), app.vault().cloned())
    else {
        println!("bench pdf no_tab");
        return bench_quit(app);
    };
    let from = pdf.key();
    let Some(stem) = from.strip_suffix(".pdf") else {
        println!("bench pdf not_a_pdf {from}");
        return bench_quit(app);
    };
    let (to, was) = (format!("{stem}-renamed.pdf"), pdf.path());
    crate::fileops::move_all(&ops, vec![(from.clone(), to.clone())]);
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
    let _ = WidgetExt::activate_action(&app.window, "win.pdf-add-page-after", None);
    written(app).await;
    let sizes = accent_core::pdf::PdfDoc::open(pdf.path())
        .and_then(|doc| doc.page_sizes())
        .unwrap_or_default();
    // On a remote vault a push that found no stamp under the new name put the page beside the
    // file instead, as `(edited)`.
    println!(
        "bench pdf renamed_added pages={} on_disk={} in_vault {} edited={}",
        pdf.page_count(),
        sizes.len(),
        vault_pages(app, &pdf.key()),
        vault.exists(&accent_api::remote::edited_name(&pdf.key(), 1))
    );
    // Rebuilt by something else, as a LaTeX run does: a one-page document written over the new
    // name, which the reader must follow rather than the copy it had.
    let rebuilt = rebuild(&vault, &pdf.key());
    let started = std::time::Instant::now();
    while rebuilt.is_ok() && pdf.page_count() != 1 && started.elapsed() < Duration::from_secs(15) {
        glib::timeout_future(Duration::from_millis(100)).await;
    }
    println!(
        "bench pdf renamed_rebuilt pages={} after_ms={} {rebuilt:?}",
        pdf.page_count(),
        started.elapsed().as_millis()
    );
    bench_quit(app);
}

/// Write a one-page document over `key` from outside the tab, as a LaTeX run rebuilding it does.
fn rebuild(vault: &accent_api::Vault, key: &str) -> anyhow::Result<()> {
    let blank = std::env::temp_dir().join(format!("accent-bench-{}-blank.pdf", std::process::id()));
    let rebuilt = accent_core::pdf::blank_pdf((595.0, 842.0))
        .and_then(|bytes| Ok(std::fs::write(&blank, bytes)?))
        .and_then(|()| Ok(vault.upload(&blank, key)?));
    let _ = std::fs::remove_file(&blank);
    rebuilt
}

/// Press the button saying `label` on the newest toast that has one, as a click on it does.
fn press_toast(app: &Rc<App>, label: &str) -> bool {
    let button = find_widget(app.toasts.widget().upcast_ref(), &|w| {
        w.downcast_ref::<gtk::Button>()
            .is_some_and(|b| b.label().as_deref() == Some(label))
    });
    button
        .and_downcast::<gtk::Button>()
        .map(|b| b.emit_clicked())
        .is_some()
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

/// The etag gate on the way back to a host: a page added to a document whose host copy has
/// moved since it was fetched must not overwrite it, and the ink must not be dropped either — it
/// goes beside the original in the vault, as `<name> (edited).pdf`, and the tab moves onto it.
///
/// The move is made by stamping the cached copy with an etag the host never had, rather than by
/// really writing on the host: a host-side write is reported by its own watcher, and the refetch
/// that follows wins the race against the save under test every time. What `push` compares is
/// the stamp against the host, so this is the same input from where it stands.
///
/// A second page is added after the first refusal, which is the reader who keeps drawing: it
/// must go into that copy as any write does, neither into a numbered one nor with a toast.
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
        // The stamp as the file spells one, written by hand because the app does not link
        // serde_json: the copy's own etag as it is, and a host's it will never report.
        let copy = accent_core::fs::Etag::of(&pdf.path());
        let moved = stamp.as_ref().zip(copy.ok()).map(|(stamp, copy)| {
            let etag = |e: accent_core::fs::Etag| {
                format!(
                    r#"{{"mtime_ns":{},"size":{},"ino":{}}}"#,
                    e.mtime_ns, e.size, e.ino
                )
            };
            let host = accent_core::fs::Etag {
                mtime_ns: 1,
                size: 1,
                ino: 1,
            };
            let stamped = format!(r#"{{"host":{},"copy":{}}}"#, etag(host), etag(copy));
            std::fs::write(stamp, stamped)
        });
        println!(
            "bench pdf stale opened pages={} in_vault {} moved={moved:?}",
            pdf.page_count(),
            vault_pages(&app, &key)
        );
        let kept = pdf.path().with_extension("kept.pdf");
        let (first, second) = (
            accent_api::remote::edited_name(&key, 1),
            accent_api::remote::edited_name(&key, 2),
        );
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-add-page-after", None);
        written(&app).await;
        println!(
            "bench pdf stale refused key={:?} pages={} in_vault {} edited {} said={} {:?} kept={}",
            pdf.key(),
            pdf.page_count(),
            vault_pages(&app, &key),
            vault_pages(&app, &first),
            app.toasted.get(),
            bench_said(&app),
            kept.exists()
        );
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-add-page-after", None);
        written(&app).await;
        println!(
            "bench pdf stale again key={:?} pages={} in_vault {} edited {} said={} numbered={}",
            pdf.key(),
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

/// The write-back's failure arm: the host will not take the upload at all, which is neither the
/// etag refusal nor a conflict copy. The document and its folder are made read-only on the host
/// behind the app's back, and a page is added twice: both saves fail, and only the first says so.
/// Then the host takes writes again and the toast's Retry is pressed, which must send both pages
/// without another stroke. A failure after that is news again; and the host's file changing
/// while that page has not gone up (a LaTeX build, here a one-page document written over it) must
/// not fetch over the page: it goes beside the original as `(edited)`, and the tab with it. Each
/// `chmod` of the folder also sets off a rescan on the host, whose "Indexed …" toast is in the
/// count: +2, +0, +1, +2 is one failure said per streak.
pub(super) fn bench_pdf_failed(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let (Some(pdf), Some(vault)) = (opened(&app, &rel).await, app.vault().cloned()) else {
            println!("bench pdf failed no_tab");
            return bench_quit(&app);
        };
        let (key, Some(remote)) = (pdf.key(), vault.remote().cloned()) else {
            println!("bench pdf failed not_remote");
            return bench_quit(&app);
        };
        let host = |mode: &str| chmod_on_host(&vault, &remote, &key, mode);
        let add = || WidgetExt::activate_action(&app.window, "win.pdf-add-page-after", None);
        let edited = accent_api::remote::edited_name(&key, 1);
        let step = |name: &str| {
            println!(
                "bench pdf failed {name} key={:?} pages={} unsent={} in_vault {} edited {} said={} \
                 {:?}",
                pdf.key(),
                pdf.page_count(),
                pdf.unsent(),
                vault_pages(&app, &key),
                vault_pages(&app, &edited),
                app.toasted.get(),
                bench_said(&app)
            )
        };
        step("opened");
        println!("bench pdf failed read_only={}", host("a-w"));
        let _ = add();
        written(&app).await;
        step("refused");
        let _ = add();
        written(&app).await;
        step("again");
        println!("bench pdf failed writable={}", host("u+w"));
        println!(
            "bench pdf failed retry pressed={}",
            press_toast(&app, "Retry")
        );
        written(&app).await;
        step("retried");
        println!("bench pdf failed read_only={}", host("a-w"));
        let _ = add();
        written(&app).await;
        step("refused_after");
        println!("bench pdf failed writable={}", host("u+w"));
        let rebuilt = rebuild(&vault, &key);
        // The host's watcher reports the write, and the tab answers it.
        let started = Instant::now();
        while pdf.key() == key
            && pdf.page_count() != 1
            && started.elapsed() < Duration::from_secs(8)
        {
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        written(&app).await;
        println!("bench pdf failed rebuilt={rebuilt:?}");
        step("rebuilt");
        bench_quit(&app);
    });
}

/// `chmod <mode>` on the host, of the document `key` and its folder, behind the app's back.
fn chmod_on_host(
    vault: &accent_api::Vault,
    remote: &accent_api::remote::Remote,
    key: &str,
    mode: &str,
) -> bool {
    let path = vault.resolve(key).unwrap_or_default();
    let (file, dir) = (
        accent_api::ssh::quote(&path.to_string_lossy()),
        accent_api::ssh::quote(&path.parent().unwrap_or(&path).to_string_lossy()),
    );
    let argv = accent_api::ssh::run(
        remote.url(),
        remote.control_path(),
        &format!("chmod {mode} {file} {dir}"),
    );
    std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .is_ok_and(|s| s.success())
}

/// The link dropped mid-draw: a page is added and the vault's ssh master ended at once, so the
/// save and the rewrite of the notes' page links land while the link is down. The automatic
/// reconnect is called off, as a refusal calls it off, to keep the link down for as long as that
/// takes. The banner says the link went, so nothing more is said; then Reconnect Now brings the
/// vault back, and the page must reach the host and the links follow it without another stroke.
/// A note linking into pages 1 to 3 is written first, so point it at a document of three pages
/// or more: the page goes in after the first, so 1, 2, 3, 2, 1 become 1, 3, 4, 3, 1.
pub(super) fn bench_pdf_dropped(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        // Opened first, which waits for the host; the note's tab then comes in front of it.
        let (Some(pdf), Some(vault)) = (opened(&app, &rel).await, app.vault().cloned()) else {
            println!("bench pdf dropped no_tab");
            return bench_quit(&app);
        };
        let (key, Some(remote)) = (pdf.key(), vault.remote().cloned()) else {
            println!("bench pdf dropped not_remote");
            return bench_quit(&app);
        };
        let note = linked_note(&app, &key).await;
        app.open_path(&key);
        let said = Cell::new(app.toasted.get());
        println!(
            "bench pdf dropped opened pages={} in_vault {} said={} {}",
            pdf.page_count(),
            vault_pages(&app, &key),
            app.toasted.get(),
            links_read(&app, &note, &said)
        );
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-add-page-after", None);
        let argv = accent_api::ssh::exit(remote.url(), remote.control_path());
        let ended = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .status()
            .is_ok_and(|s| s.success());
        // The banner is up once the window has heard; that is when its countdown can go.
        for _ in 0..50 {
            if app.connection.is_revealed() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        app.connection_refused("the drill holds the link down");
        written(&app).await;
        println!(
            "bench pdf dropped down ended={ended} offline={} pages={} unsent={} said={} {:?}",
            app.offline(),
            pdf.page_count(),
            pdf.unsent(),
            app.toasted.get(),
            bench_said(&app)
        );
        said.set(app.toasted.get());
        app.reconnect_now();
        for _ in 0..200 {
            if !app.offline() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        written(&app).await;
        println!(
            "bench pdf dropped back offline={} pages={} unsent={} in_vault {} said={} {}",
            app.offline(),
            pdf.page_count(),
            pdf.unsent(),
            vault_pages(&app, &key),
            app.toasted.get(),
            links_read(&app, &note, &said)
        );
        bench_quit(&app);
    });
}

/// A drawn-on PDF closed at once: a page is added and the tab closed in the same turn, so the
/// write the close flushes lands after the tab has gone. It must still reach the vault, which on a
/// remote vault is an upload nobody is left to ask for. On a remote vault the same again with
/// the host's folder read-only, so the page cannot go: it must go up when the document is next
/// opened, the host taking writes again by then.
pub(super) fn bench_pdf_closed(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let Some(pdf) = opened(&app, &rel).await else {
            println!("bench pdf closed no_tab");
            return bench_quit(&app);
        };
        let (key, page) = (pdf.key(), pdf.page.clone());
        println!(
            "bench pdf closed opened pages={} in_vault {}",
            pdf.page_count(),
            vault_pages(&app, &key)
        );
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-add-page-after", None);
        // The drill's own hold on the tab would keep it alive past the close.
        drop(pdf);
        app.close_page(&page);
        written(&app).await;
        println!(
            "bench pdf closed gone open={} in_vault {} said={} {:?}",
            app.doc_for(&key).is_some(),
            vault_pages(&app, &key),
            app.toasted.get(),
            bench_said(&app)
        );
        let Some(remote) = app.vault().and_then(|v| v.remote()).cloned() else {
            return bench_quit(&app);
        };
        let vault = app.vault().cloned().expect("a remote vault");
        let Some(pdf) = opened(&app, &key).await else {
            println!("bench pdf closed no_tab");
            return bench_quit(&app);
        };
        let page = pdf.page.clone();
        println!(
            "bench pdf closed read_only={}",
            chmod_on_host(&vault, &remote, &key, "a-w")
        );
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-add-page-after", None);
        drop(pdf);
        app.close_page(&page);
        written(&app).await;
        println!(
            "bench pdf closed refused in_vault {} said={} {:?}",
            vault_pages(&app, &key),
            app.toasted.get(),
            bench_said(&app)
        );
        println!(
            "bench pdf closed writable={}",
            chmod_on_host(&vault, &remote, &key, "u+w")
        );
        let pdf = opened(&app, &key).await;
        written(&app).await;
        println!(
            "bench pdf closed reopened pages={:?} in_vault {} said={}",
            pdf.map(|pdf| pdf.page_count()),
            vault_pages(&app, &key),
            app.toasted.get()
        );
        bench_quit(&app);
    });
}

/// A remote PDF renamed while its upload is still on the way: a page is added, and the file is
/// renamed two seconds later, which is inside the upload for a document of a few megabytes. The
/// upload must not bring the old name back on the host, and the page must end up in the renamed
/// file, not beside it as `(edited)`.
pub(super) fn bench_pdf_renaming(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let (Some(pdf), Some(vault), Some(ops)) = (
            opened(&app, &rel).await,
            app.vault().cloned(),
            app.ops().cloned(),
        ) else {
            println!("bench pdf renaming no_tab");
            return bench_quit(&app);
        };
        let from = pdf.key();
        let Some(to) = from
            .strip_suffix(".pdf")
            .map(|stem| format!("{stem}-renamed.pdf"))
        else {
            println!("bench pdf renaming not_a_pdf {from}");
            return bench_quit(&app);
        };
        println!(
            "bench pdf renaming opened pages={} in_vault {}",
            pdf.page_count(),
            vault_pages(&app, &from)
        );
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-add-page-after", None);
        glib::timeout_future(Duration::from_secs(2)).await;
        crate::fileops::move_all(&ops, vec![(from.clone(), to.clone())]);
        glib::timeout_future(Duration::from_secs(20)).await;
        let edited = [&from, &to]
            .into_iter()
            .any(|key| vault.exists(&accent_api::remote::edited_name(key, 1)));
        println!(
            "bench pdf renaming settled key={:?} pages={} in_vault {} old_back={} edited={edited} \
             unsent={} said={}",
            pdf.key(),
            pdf.page_count(),
            vault_pages(&app, &to),
            vault.exists(&from),
            pdf.unsent(),
            app.toasted.get()
        );
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
        online(&app).await;
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
            "bench drawing made key={:?} tool={:?} window={}x{} on_disk={sizes:?} in_vault {}",
            pdf.key(),
            pdf.mode_label(),
            app.window.width(),
            app.window.height(),
            vault_pages(&app, &pdf.key()),
        );
        bench_quit(&app);
    });
}

/// Insert Sketch from the note `rel`: what the note gained, the tab it opened beside it with the
/// tool in hand, and what the vault holds under that name — on a remote vault the host's copy.
pub(super) fn bench_sketch(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        online(&app).await;
        app.open_path(&rel);
        let Some(tab) = until(|| app.tab_for(&rel)).await else {
            println!("bench sketch no_note");
            return bench_quit(&app);
        };
        let before = tab.text();
        let started = Instant::now();
        let _ = WidgetExt::activate_action(&app.window, "win.insert-sketch", None);
        let held = started.elapsed().as_millis();
        let pdf = until(|| app.active_pdf().filter(|pdf| pdf.page_count() > 0)).await;
        let Some(pdf) = pdf else {
            println!(
                "bench sketch no_tab held_ms={held} note_changed={} said={} {:?}",
                tab.text() != before,
                app.toasted.get(),
                bench_said(&app)
            );
            return bench_quit(&app);
        };
        println!(
            "bench sketch made held_ms={held} after_ms={} key={:?} embedded={} tool={:?} \
             in_vault {} said={}",
            started.elapsed().as_millis(),
            pdf.key(),
            tab.text().contains(&format!("![[{}]]", pdf.key())),
            pdf.mode_label(),
            vault_pages(&app, &pdf.key()),
            app.toasted.get()
        );
        bench_quit(&app);
    });
}

/// Once a remote vault answers; a local one at once.
pub(super) async fn online(app: &Rc<App>) {
    for _ in 0..150 {
        if !app.offline() {
            return;
        }
        glib::timeout_future(Duration::from_millis(100)).await;
    }
}
