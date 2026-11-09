//! PDF drills for layout, page editing, drops and rendering, and the helpers PDF drills share.

use super::*;

/// Check Fit Height and Add Page After through window actions from halfway down page 2. Reopen
/// the saved file to verify the added page reaches disk, as another reader would see it.
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

/// The pages exact search finds `text` on in `rel`, once they are `want` or after five seconds.
async fn found_on(app: &Rc<App>, rel: &str, text: &str, want: &[u32]) -> Vec<u32> {
    let mut pages = Vec::new();
    for _ in 0..50 {
        let found = (app.vault()).and_then(|v| v.grep(text, Default::default(), 50, false).ok());
        let rows = found.map(|(rows, _)| rows).unwrap_or_default();
        pages = (rows.iter())
            .filter(|row| row.rel_path == rel)
            .filter_map(|row| row.page)
            .collect();
        if pages == want {
            break;
        }
        glib::timeout_future(Duration::from_millis(100)).await;
    }
    pages
}

/// Exercise move, insert-before, append and delete through the strip call and window actions,
/// reopening disk after each, then Undo and Redo all four. A note links to pages 1–3 through
/// highlights, jumps, markdown, HTML and reference definitions; report file/buffer link rewrites
/// and toasts, including rename between deletion and Undo. Opening before initial indexing
/// reports `indexed=false`; rewrites wait for the walk.
///
/// Check context-menu page targeting while another page is read, undo both commands, and report
/// selection/no-selection page menus and the page-count menu. Real drag, hover controls, drop
/// bar and edge scrolling require pointer tests.
pub(super) fn bench_pdf_pages(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        // A remote vault's note is written once the host answers.
        online(&app).await;
        let note = RefCell::new(linked_note(&app, &rel).await);
        let Some(pdf) = opened(&app, &rel).await else {
            println!("bench pages no_tab");
            return bench_quit(&app);
        };
        let said = Cell::new(app.toasted.get());
        let links = || links_read(&app, &note.borrow(), &said);
        let indexed = app.reconciled.get();
        println!(
            "bench pages opened indexed={indexed} {} {}",
            pages_read(&pdf),
            links()
        );
        pdf.edit_pages(accent_core::pdf::PageEdit::Move { from: 0, to: 2 });
        written(&app).await;
        println!("bench pages moved {} {}", pages_read(&pdf), links());
        // A page edit changes the file's text, so the text pass reads it again: search finds the
        // first page's words on the page it went to.
        let found = found_on(&app, &rel, "Page 1", &[3]).await;
        println!("bench pages moved_text page_1_on={found:?}");
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
            // Refused while the vault is still being indexed.
            if app.vault().is_some_and(|v| v.exists(&to)) {
                note.replace(to);
            }
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

/// `arg=<rel>,<source rel>` tests PDF insertion through tree-to-page XTEST drag, external
/// file-drop target, tree-row append and external-row append, with Undo/Redo and a self-drop
/// that inserts nothing. Both PDFs are at the vault root; destination has several pages and a
/// note linking pages 1–3.
///
/// Observe the drop line while resting and after release. Report disk pages, link rewrites,
/// toasts and vault page counts, including the host copy remotely. Xvfb cannot drag between
/// processes, so external drops invoke the target directly.
pub(super) fn bench_pdf_insert(app: &Rc<App>, arg: &str) {
    let Some((rel, from)) = arg.split_once(',') else {
        return bench_quit(app);
    };
    let (app, rel, from) = (app.clone(), rel.to_string(), from.to_string());
    glib::spawn_future_local(async move {
        // A remote vault's note is written once the host answers.
        online(&app).await;
        let note = linked_note(&app, &rel).await;
        let Some(pdf) = opened(&app, &rel).await else {
            println!("bench insert no_tab");
            return bench_quit(&app);
        };
        // Small enough that the first pages and the gaps between them are on screen.
        pdf.set_zoom(pdfview::PdfZoom::Scale(0.25));
        glib::timeout_future(Duration::from_millis(500)).await;
        let said = Cell::new(app.toasted.get());
        let step = |what: &str| {
            let told = app.toasts.shown().into_iter();
            let told: Vec<String> = told.filter(|t| t.contains("nsert")).collect();
            println!(
                "bench insert {what} {} {} told={told:?} in_vault={}",
                pages_read(&pdf),
                links_read(&app, &note, &said),
                vault_pages(&app, &pdf.key())
            );
        };
        let walk = async |action: &str, what: &str| {
            let _ = WidgetExt::activate_action(&app.window, action, None);
            written(&app).await;
            step(what);
        };
        step("opened");
        let window = app.window.clone().upcast::<gtk::Widget>();
        let (dx, dy) = app.window.surface_transform();
        let screen = |(x, y): (f32, f32)| format!("{} {}", f64::from(x) + dx, f64::from(y) + dy);
        let row = |rel: &str| tree_row(&app, rel).map(screen).unwrap_or_default();
        let gap = |gap| pdf.drop_point(gap, &window).map(screen).unwrap_or_default();

        let lines = dragged(&pdf, &drag_steps(&row(&from), &gap(1))).await;
        written(&app).await;
        let left = pdf.drop_shown();
        step(&format!("tree_pages lines={lines:?} left={left:?}"));
        walk("win.pdf-undo", "tree_pages_undo").await;
        walk("win.pdf-redo", "tree_pages_redo").await;
        walk("win.pdf-undo", "tree_pages_undo_again").await;

        // The source's bytes outside the vault, as a file manager would hold them.
        let outside = std::env::temp_dir().join(&from);
        if let Some(Err(e)) = app.vault().map(|v| v.download(&from, &outside)) {
            println!("bench insert no_outside_copy {e}");
        }
        let view = pdf.key_target();
        let at = pdf.drop_point(0, &view).unwrap_or_default();
        let taken = file_drop(&view, &[&outside], at);
        written(&app).await;
        step(&format!("outside_pages taken={taken}"));
        walk("win.pdf-undo", "outside_pages_undo").await;

        super::git::xtest(&drag_steps(&row(&from), &row(&rel))).await;
        written(&app).await;
        step("tree_row");
        walk("win.pdf-undo", "tree_row_undo").await;

        let target = tree_row_widget(&app, &rel);
        let taken = target.is_some_and(|row| file_drop(&row, &[&outside], (1.0, 1.0)));
        written(&app).await;
        step(&format!("outside_row taken={taken}"));
        walk("win.pdf-undo", "outside_row_undo").await;

        let lines = dragged(&pdf, &drag_steps(&row(&rel), &gap(1))).await;
        written(&app).await;
        step(&format!("own_row lines={lines:?}"));
        bench_quit(&app);
    });
}

/// A press on `from`, a drag in steps to `to` and a rest there before the release, as XTEST
/// steps: the rest is what the line is watched in.
pub(super) fn drag_steps(from: &str, to: &str) -> String {
    let point = |at: &str| -> (f64, f64) {
        let mut xy = at.split(' ').filter_map(|v| v.parse().ok());
        (xy.next().unwrap_or(0.0), xy.next().unwrap_or(0.0))
    };
    let ((x0, y0), (x1, y1)) = (point(from), point(to));
    let moves: Vec<String> = (1..=12)
        .map(|i| {
            let t = f64::from(i) / 12.0;
            let (x, y) = (x0 + (x1 - x0) * t, y0 + (y1 - y0) * t);
            format!("move {x} {y}; sleep 0.03")
        })
        .collect();
    format!(
        "move {x0} {y0}; sleep 0.2; down; sleep 0.1; {}; sleep 0.8; up",
        moves.join("; ")
    )
}

/// Run `steps` through XTEST, and say which gaps of `pdf` showed the drop line meanwhile.
async fn dragged(pdf: &Rc<pdftab::PdfTab>, steps: &str) -> Vec<usize> {
    let seen = Rc::new(RefCell::new(std::collections::BTreeSet::new()));
    let watching = Rc::new(Cell::new(true));
    glib::timeout_add_local(Duration::from_millis(30), {
        let (pdf, seen, watching) = (pdf.clone(), seen.clone(), watching.clone());
        move || {
            if let Some(gap) = pdf.drop_shown() {
                seen.borrow_mut().insert(gap);
            }
            match watching.get() {
                true => glib::ControlFlow::Continue,
                false => glib::ControlFlow::Break,
            }
        }
    });
    super::git::xtest(steps).await;
    watching.set(false);
    seen.take().into_iter().collect()
}

/// The Files tree's row for `rel`, while it is on screen.
fn tree_row_widget(app: &Rc<App>, rel: &str) -> Option<gtk::Widget> {
    let tree = app.tree.get()?;
    let row = tree::expanders(tree.view()).into_iter().find(|e| {
        let item = e.list_row().and_then(|r| r.item());
        item.as_ref()
            .and_then(tree::decode)
            .is_some_and(|r| r.rel == rel)
    })?;
    Some(row.upcast())
}

/// Where on `rel`'s row in the Files tree to press, in the window: on its name.
pub(super) fn tree_row(app: &Rc<App>, rel: &str) -> Option<(f32, f32)> {
    let row = tree_row_widget(app, rel)?;
    let at = graphene::Point::new(40.0, row.height() as f32 / 2.0);
    let at = row.compute_point(&app.window, &at)?;
    Some((at.x(), at.y()))
}

/// A drop of `files` from another application at `(x, y)` of `widget`, through its own file drop
/// target: Xvfb carries a drag inside one process and not between two.
pub(super) fn file_drop(widget: &gtk::Widget, files: &[&std::path::Path], at: (f32, f32)) -> bool {
    let files: Vec<gio::File> = files.iter().map(gio::File::for_path).collect();
    value_drop(widget, gdk::FileList::from_array(&files).to_value(), at)
}

/// A drop of `value` at `(x, y)` of `widget`, through its own drop target for that type, as
/// [`file_drop`] drops files.
pub(super) fn value_drop(widget: &gtk::Widget, value: glib::Value, (x, y): (f32, f32)) -> bool {
    let target = widget
        .observe_controllers()
        .into_iter()
        .flatten()
        .filter_map(|c| c.downcast::<gtk::DropTarget>().ok())
        .find(|t| t.formats().is_some_and(|f| f.contains_type(value.type_())));
    let Some(target) = target else {
        return false;
    };
    let value = glib::BoxedValue(value);
    target.emit_by_name::<bool>("drop", &[&value, &f64::from(x), &f64::from(y)])
}

/// The thumbnail strip held on screen for XTEST to hover and drag along: the Outline pane up, and
/// the PDF in front, which Go to File may change, its page being read and its file's pages
/// printed every two seconds for 40 s.
pub(super) fn bench_pdf_strip(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let Some(pdf) = opened(&app, &rel).await else {
            println!("bench strip no_tab");
            return bench_quit(&app);
        };
        app.show_pane("outline");
        for _ in 0..20 {
            let front = app.active_pdf().unwrap_or_else(|| pdf.clone());
            let buttons = front.strip_buttons(app.window.upcast_ref());
            println!(
                "bench strip file={} {} {buttons}",
                front.key(),
                pages_read(&front)
            );
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

/// `arg=<rel>[,<percent>]` checks high-zoom cache churn, default 800%. Place the page break
/// mid-view and report wanted/missing/blurry/landed/repeated tiles each second for 20 s. Then
/// wheel by one tenth viewport every 100 ms for 10 s and Page Down ten times 600 ms apart,
/// counting steps with unsharp visible tiles.
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

/// Cut `arg` in half, open it, then restore it in six pieces 250 ms apart to test settled
/// reload. After opening, move five pages on, truncate and repair again. Report status, reading
/// page, delay from final write and open attempts; six writes must cause one reopen. Accept
/// vault-relative or external absolute paths. Modifies the file; use a scratch PDF with at least
/// twelve pages.
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
        let Some(pdf) = until("pdf", "broken wait", 10_000, failed).await else {
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
        until("pdf", "written", 10_000, || {
            (pdf.page_count() > 0).then_some(())
        })
        .await;
        say(&pdf, "written", written);
        pdf.goto_page(pdf.page_count() / 2);
        glib::timeout_future(Duration::from_secs(1)).await;
        let _ = std::fs::write(&file, half);
        let broken = Instant::now();
        // A reader still reading, whose next page is what finds the file changed under it where
        // no watcher says so.
        pdf.goto_page(pdf.current_page() + 5);
        until("pdf", "broken", 10_000, || pdf.waiting().0.map(drop)).await;
        say(&pdf, "broken", broken);
        let _ = std::fs::write(&file, &whole);
        let mended = Instant::now();
        until("pdf", "mended", 10_000, || {
            (pdf.page_count() > 0).then_some(())
        })
        .await;
        say(&pdf, "mended", mended);
        bench_quit(&app);
    });
}

/// Wait for both views of `pdf` to have painted everything they want and print how long that
/// took, or, once what is missing has not changed in five seconds, print it: true when it did not.
pub(super) async fn settled(pdf: &Rc<pdftab::PdfTab>, what: &str) -> bool {
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

/// The page being read, and what each page of the file on disk says and where its bookmarks lead:
/// what a second reader opens.
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
    let doc = accent_core::pdf::PdfDoc::open(pdf.path());
    let on_disk: Vec<String> = doc
        .as_ref()
        .map(|doc| (0..doc.page_count()).map(|p| text(doc, p)).collect())
        .unwrap_or_default();
    // The page each bookmark leads to, from one: a page edit must not leave one leading nowhere.
    let marks: Vec<Option<usize>> = doc
        .and_then(|doc| doc.outline())
        .map(|marks| marks.iter().map(|m| m.page.map(|p| p + 1)).collect())
        .unwrap_or_default();
    format!(
        "reading={} of {} on_disk={on_disk:?} marks={marks:?}",
        pdf.current_page() + 1,
        pdf.page_count()
    )
}

/// Write `Page links.md` at the vault root, linking into pages 1 to 3 of `pdf` as a highlight,
/// a jump, a markdown link, an HTML `href` into page 2 and a reference definition into page 1,
/// wait until the index has it as one of the PDF's backlinks, and open
/// it in a tab, which a rewrite has to reload.
pub(super) async fn linked_note(app: &Rc<App>, pdf: &str) -> String {
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
    let linked = || {
        let linked = vault
            .backlinks(pdf)
            .is_ok_and(|links| links.iter().any(|b| b.src_rel_path == note));
        // A vault still being indexed has it only once the walk is done, which the page edits'
        // rewrites wait for themselves.
        linked || !app.reconciled.get()
    };
    until("pages", "backlink", 10_000, linked).await;
    app.open_path(&note);
    until("pages", "note tab", 5000, || app.tab_for(&note).is_some()).await;
    note
}

/// The pages the note's links name, in the order it holds them — on disk, and in its tab unless
/// the tab holds the same — and what the toast said since the last time this was asked: how the
/// notes followed the step before it.
pub(super) fn links_read(app: &Rc<App>, note: &str, said: &Cell<usize>) -> String {
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
pub(super) async fn written(app: &Rc<App>) {
    glib::timeout_future(Duration::from_millis(1400)).await;
    if app.vault().is_some_and(|v| v.is_remote()) {
        glib::timeout_future(Duration::from_millis(2000)).await;
    }
}

/// Rename an open PDF through the move path, add a page, reopen disk and overwrite it externally
/// with one page. Verify the render thread follows its new save/reload path. Remote cached
/// copies must move too, avoiding an `(edited)` copy beside the renamed original.
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
pub(super) fn rebuild(vault: &accent_api::Vault, key: &str) -> anyhow::Result<()> {
    let blank = std::env::temp_dir().join(format!("accent-bench-{}-blank.pdf", std::process::id()));
    let rebuilt = accent_core::pdf::blank_pdf((595.0, 842.0))
        .map_err(anyhow::Error::from)
        .and_then(|bytes| Ok(std::fs::write(&blank, bytes)?))
        .and_then(|()| Ok(vault.upload(&blank, key, true)?));
    let _ = std::fs::remove_file(&blank);
    rebuilt
}

/// What the vault's own copy of `key` holds, fetched past the cache the reader is drawing on.
pub(super) fn vault_pages(app: &Rc<App>, key: &str) -> String {
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

/// How many PDF render threads this process runs.
pub(super) fn render_threads() -> usize {
    render_tasks().len()
}

/// How long the PDF render threads have run on a CPU, in ms: `schedstat`'s first field is ns.
pub(super) fn render_cpu_ms() -> f64 {
    let ran = |task: &std::path::PathBuf| {
        let stat = std::fs::read_to_string(task.join("schedstat")).ok()?;
        stat.split_whitespace().next()?.parse::<f64>().ok()
    };
    render_tasks().iter().filter_map(ran).sum::<f64>() / 1e6
}

/// The PDF render threads' entries under `/proc/self/task`.
fn render_tasks() -> Vec<std::path::PathBuf> {
    std::fs::read_dir("/proc/self/task")
        .into_iter()
        .flatten()
        .flatten()
        .map(|task| task.path())
        .filter(|task| {
            std::fs::read_to_string(task.join("comm")).is_ok_and(|c| c.trim() == "accent-pdf")
        })
        .collect()
}

/// Every label's text under `w`, the selectable ones marked `*`.
pub(super) fn labels(w: &gtk::Widget) -> Vec<String> {
    let mut out: Vec<String> = w
        .downcast_ref::<gtk::Label>()
        .map(|l| format!("{}{}", l.label(), if l.is_selectable() { "*" } else { "" }))
        .into_iter()
        .collect();
    let mut child = w.first_child();
    while let Some(c) = child {
        out.extend(labels(&c));
        child = c.next_sibling();
    }
    out
}

/// Once a remote vault answers; a local one at once.
pub(super) async fn online(app: &Rc<App>) {
    until("remote", "online", 15_000, || !app.offline()).await;
}
