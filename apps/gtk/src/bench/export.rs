//! Read back exports and print copies, bypassing file chooser and print-system input.

use super::*;
use accent_core::pdf::{InkStyle, PdfDoc};
use std::path::Path;

/// `pdf:<rel_pdf>`: see [`bench_export_pdf`]; `note:<rel_note>` and `notebook:<rel_notebook>`:
/// see [`bench_export_html`]; `stall:<rel_note>,<rel_notebook>,<dir>[,flat]`: see
/// [`bench_export_stall`], `flat` leaving the diagram's pictures out.
pub(super) fn bench_export(app: &Rc<App>, arg: &str) {
    match arg.split_once(':') {
        Some(("pdf", rel)) => bench_export_pdf(app, rel),
        Some(("note", rel)) => bench_export_html(app, rel, "export_html"),
        Some(("notebook", rel)) => bench_export_html(app, rel, "export_notebook"),
        Some(("stall", rels)) => match rels.split(',').collect::<Vec<_>>()[..] {
            [note, notebook, dir, ref flat @ ..] => {
                bench_export_stall(app, note, notebook, Path::new(dir), flat == ["flat"])
            }
            _ => {
                println!("bench export_stall wants <rel_note>,<rel_notebook>,<dir>[,flat]");
                bench_quit(app);
            }
        },
        _ => {
            println!("bench export unknown {arg:?}");
            bench_quit(app);
        }
    }
}

/// Add ink and two linked selections to a PDF, export into `$TMPDIR`, reject exporting
/// over the source, and inspect Print's copy before cancelling its dialog. Compare
/// pages, highlights and ink; require source bytes unchanged by export/print.
/// Fixture setup writes the source, so use `make vault VAULT=/tmp/<name>`.
fn bench_export_pdf(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let Some(vault) = app.vault().cloned() else {
            println!("bench export_pdf no_vault");
            return bench_quit(&app);
        };
        let source = vault.root().join(&rel);
        if let Err(e) = draw_a_stroke(&source) {
            println!("bench export_pdf no_stroke {e:#}");
        }
        let note = "Export links.md";
        let text = links_into(&source, &rel).unwrap_or_default();
        if let Err(e) = vault.save(note, &text, None) {
            println!("bench export_pdf note_not_written {e:?}");
        }
        let linked = || {
            vault
                .backlinks(&rel)
                .is_ok_and(|links| links.iter().any(|b| b.src_rel_path == note))
        };
        until("export_pdf", "backlink", 10_000, linked).await;
        let Some(pdf) = super::pdf::opened(&app, &rel).await else {
            println!("bench export_pdf no_tab");
            return bench_quit(&app);
        };
        until("export_pdf", "note links", 5000, || pdf.has_note_links()).await;
        let before = std::fs::read(&source).unwrap_or_default();
        let unchanged = || std::fs::read(&source).is_ok_and(|now| now == before);
        let (pages, highlights, ink) = counts(&source);

        let dest = std::env::temp_dir().join(format!(
            "{} (exported).pdf",
            rel.trim_end_matches(".pdf")
                .rsplit('/')
                .next()
                .unwrap_or_default()
        ));
        let _ = std::fs::remove_file(&dest);
        crate::pdf::export::export_to(&app, &pdf, dest.clone());
        let busy = app.statusbar.progress_text();
        let said = toast(&app, "Exported ").await;
        let (copy_pages, copy_highlights, copy_ink) = counts(&dest);
        println!(
            "bench export_pdf pages={pages}/{copy_pages} highlights={highlights}→{copy_highlights} \
             ink={ink}/{copy_ink} source_unchanged={} busy={busy:?} said={said:?}",
            unchanged()
        );

        crate::pdf::export::export_to(&app, &pdf, source.clone());
        let said = toast(&app, "Cannot ").await;
        println!(
            "bench export_pdf_onto_itself said={said:?} source_unchanged={}",
            unchanged()
        );

        match crate::pdf::export::print_copy(&pdf).await {
            Ok(file) => {
                let (pages, highlights, ink) = counts(&file);
                println!("bench print_pdf_source pages={pages} highlights={highlights} ink={ink}");
                bench_print_dialog(&app, &pdf, &file).await;
            }
            Err(why) => println!("bench print_pdf_source failed={why}"),
        }
        println!("bench export_pdf done source_unchanged={}", unchanged());
        bench_quit(&app);
    });
}

/// Print…, as far as its dialog: whether one comes up, and whether the copy it was handed is
/// gone once it is closed unanswered, with no toast.
async fn bench_print_dialog(app: &Rc<App>, pdf: &Rc<pdftab::PdfTab>, file: &Path) {
    let (before, toasted) = (gtk::Window::list_toplevels(), app.toasted.get());
    crate::pdf::export::print(app, pdf);
    let mut dialog = None;
    for _ in 0..50 {
        dialog = gtk::Window::list_toplevels()
            .into_iter()
            .find(|w| w.is_visible() && !before.contains(w))
            .and_downcast::<gtk::Window>();
        if dialog.is_some() {
            break;
        }
        glib::timeout_future(Duration::from_millis(100)).await;
    }
    let kind = dialog.as_ref().map(|d| d.type_().name().to_string());
    let busy = app.statusbar.progress_text();
    if let Some(dialog) = dialog {
        dialog.close();
    }
    for _ in 0..30 {
        if !file.exists() {
            break;
        }
        glib::timeout_future(Duration::from_millis(100)).await;
    }
    glib::timeout_future(Duration::from_millis(300)).await;
    let said = match app.toasted.get() == toasted {
        true => None,
        false => toast(app, "Cannot print").await,
    };
    println!(
        "bench print_pdf dialog={kind:?} copy_gone={} busy={busy:?} busy_after={:?} said={said:?}",
        !file.exists(),
        app.statusbar.progress_text()
    );
}

/// Export a note or a notebook as HTML into `$TMPDIR` and read it back, printed as `name`: its
/// pictures inlined, its tables and formulas, and nothing of the app or of a script left in it.
fn bench_export_html(app: &Rc<App>, rel: &str, name: &'static str) {
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let mut paper = None;
        for _ in 0..100 {
            paper = app
                .doc_for(&rel)
                .as_ref()
                .and_then(crate::export::Paper::of);
            if paper.is_some() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        let Some(paper) = paper else {
            println!("bench {name} no_tab");
            return bench_quit(&app);
        };
        let stem = Path::new(&rel)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let html_path = std::env::temp_dir().join(format!("{stem}.html"));
        let _ = std::fs::remove_file(&html_path);
        app.html_to(paper, html_path.clone());
        let said = toast(&app, &format!("Exported {stem}.html"))
            .await
            .or_else(|| bench_said(&app));
        let html = std::fs::read_to_string(&html_path).unwrap_or_default();
        println!(
            "bench {name} accent_refs={} data_images={} web_images={} svg={} tables={} math={} \
             script={} csp={} said={said:?}",
            html.matches("accent:").count(),
            html.matches("src=\"data:").count(),
            html.matches("src=\"https:").count(),
            html.matches("<svg").count(),
            html.matches("<table").count(),
            html.matches("<math").count(),
            html.matches("<script").count(),
            html.contains("script-src 'none'"),
        );
        bench_quit(&app);
    });
}

/// One pen stroke on the first page, written into the file itself.
fn draw_a_stroke(path: &Path) -> anyhow::Result<()> {
    let mut doc = PdfDoc::open(path)?;
    let pen = InkStyle {
        width: 2.0,
        rgba: [0, 0, 255, 255],
        multiply: false,
    };
    doc.add_ink(0, &[(60.0, 200.0), (200.0, 240.0)], pen, 1.0)?;
    let bytes = doc.save()?;
    drop(doc);
    std::fs::write(path, bytes)?;
    Ok(())
}

/// Two links to selections on the first page, its first five characters and its last six, as
/// Copy Link to Selection writes them.
fn links_into(path: &Path, rel: &str) -> anyhow::Result<String> {
    let glyphs = PdfDoc::open(path)?.page_text(0)?;
    let n = glyphs.len();
    let link = |start: usize, end: usize| {
        let sel = accent_core::pdf::Selection {
            page: 0,
            start,
            end,
        };
        let out = accent_core::pdf::selection_link(&glyphs, rel, &sel);
        accent_core::pdf::link_with_alias(&out.link, &out.text)
    };
    Ok(format!("{} {}\n", link(0, 5), link(n.saturating_sub(6), n)))
}

/// A PDF's pages, `/Highlight`s and ink strokes; zeros for one that does not open.
fn counts(path: &Path) -> (usize, usize, usize) {
    let Ok(doc) = PdfDoc::open(path) else {
        return (0, 0, 0);
    };
    let pages = doc.page_count();
    let highlights = doc.highlights().map_or(0, |h| h.len());
    let ink = (0..pages).map(|p| doc.inks(p).map_or(0, |i| i.len())).sum();
    (pages, highlights, ink)
}

/// What the toast starting with `prefix` says, once one is up.
pub(super) async fn toast(app: &Rc<App>, prefix: &str) -> Option<String> {
    for _ in 0..100 {
        let label = find_widget(app.window.upcast_ref(), &|w| {
            w.downcast_ref::<gtk::Label>()
                .is_some_and(|l| l.label().starts_with(prefix))
        });
        if let Some(label) = label.and_downcast::<gtk::Label>() {
            return Some(label.label().to_string());
        }
        glib::timeout_future(Duration::from_millis(100)).await;
    }
    None
}

/// How many pages [`bench_export_stall`]'s diagram has, and how many boxes on each.
const STALL_PAGES: usize = 12;
const STALL_BOXES: usize = 196;

/// A large diagram for [`bench_export_stall`]: `count` pages of [`STALL_BOXES`] labelled boxes,
/// each joined to the one before, and two formulas; unless `flat`, two photo-like PNGs and two SVG
/// charts on each page too.
fn stall_diagram(count: usize, flat: bool) -> String {
    use super::diagram::pictures::{png_picture, svg_picture};
    let mut pages = String::new();
    for p in 0..count {
        let mut cells = String::new();
        for i in 0..STALL_BOXES {
            let (x, y) = (20 + i % 14 * 110, 20 + i / 14 * 80);
            cells.push_str(&format!(
                r#"<mxCell id="v{i}" value="Step {i}&lt;br&gt;&lt;b&gt;page {p}&lt;/b&gt;" style="rounded=1;whiteSpace=wrap;html=1;fillColor=#dae8fc;strokeColor=#6c8ebf;" parent="1" vertex="1"><mxGeometry x="{x}" y="{y}" width="90" height="50" as="geometry" /></mxCell>"#
            ));
            if i > 0 {
                cells.push_str(&format!(
                    r#"<mxCell id="e{i}" style="edgeStyle=orthogonalEdgeStyle;rounded=0;html=1;" parent="1" source="v{}" target="v{i}" edge="1"><mxGeometry relative="1" as="geometry" /></mxCell>"#,
                    i - 1
                ));
            }
        }
        for k in (0..4).filter(|_| !flat) {
            let n = p * 4 + k;
            let uri = match k % 2 {
                0 => format!("data:image/png,{}", glib::base64_encode(&png_picture(n))),
                _ => format!(
                    "data:image/svg+xml,{}",
                    glib::base64_encode(svg_picture(n).as_bytes())
                ),
            };
            cells.push_str(&format!(
                r#"<mxCell id="p{k}" value="" style="shape=image;imageAspect=0;image={uri};" parent="1" vertex="1"><mxGeometry x="{}" y="1150" width="320" height="240" as="geometry" /></mxCell>"#,
                20 + k * 380
            ));
        }
        cells.push_str(
            r#"<mxCell id="m0" value="\(x_{i}^2 + \sqrt{y}\)" style="text;html=1;" parent="1" vertex="1"><mxGeometry x="20" y="1420" width="200" height="30" as="geometry" /></mxCell><mxCell id="m1" value="$$\frac{a}{b} = \sum_{k=0}^{n} k$$" style="text;html=1;" parent="1" vertex="1"><mxGeometry x="400" y="1420" width="200" height="60" as="geometry" /></mxCell>"#,
        );
        pages.push_str(&format!(
            r#"<diagram name="Page {p}" id="stall-{p}"><mxGraphModel page="1" pageWidth="1600" pageHeight="1500" math="1"><root><mxCell id="0" /><mxCell id="1" parent="0" />{cells}</root></mxGraphModel></diagram>"#
        ));
    }
    format!("<mxfile host=\"accent\">{pages}</mxfile>\n")
}

/// What one export did to the window: the longest the main loop went without a turn and its
/// busy total (`native::Beat`), how long until it was done, and each state of the status bar's
/// line and of the bar over the document column in turn, read every frame. The first frame that
/// counts two pages done with the bar up is pictured into `shot`, where one is given.
struct Watch {
    beat: super::native::Beat,
    started: Instant,
    toasted: usize,
    seen: Rc<RefCell<Vec<String>>>,
    tick: gtk::TickCallbackId,
}

impl Watch {
    fn start(app: &Rc<App>, shot: Option<PathBuf>) -> Watch {
        let seen: Rc<RefCell<Vec<String>>> = Rc::default();
        let shot = Cell::new(shot);
        let tick = app.window.add_tick_callback({
            let (app, seen) = (app.clone(), seen.clone());
            move |_, _| {
                let (line, bar) = (app.statusbar.progress_text(), app.connect.shown());
                let counting = line.ends_with(" 2/12 pages") && bar != "off";
                if counting && let Some(path) = shot.take() {
                    let window = app.window.clone();
                    glib::idle_add_local_once(move || {
                        let shot = super::diagram::shoot(window.upcast_ref(), &path);
                        println!("bench export_stall shot={shot} {path:?}");
                    });
                }
                // The fraction moves every page, so a bar that counts is said once.
                let bar = match bar.parse::<f64>() {
                    Ok(_) => "counts",
                    Err(_) => bar.as_str(),
                };
                let line = line.split('…').next().unwrap_or_default();
                let state = format!("{line}|{bar}");
                let mut seen = seen.borrow_mut();
                if seen.last() != Some(&state) {
                    seen.push(state);
                }
                glib::ControlFlow::Continue
            }
        });
        Watch {
            beat: super::native::Beat::start(),
            started: Instant::now(),
            toasted: app.toasted.get(),
            seen,
            tick,
        }
    }

    /// Once a toast has come, or `ready` says the work is done: `bench export_stall <what> …`,
    /// with how many pages the PDF at `out` has.
    async fn report(self, app: &Rc<App>, what: &str, out: &Path, ready: impl Fn() -> bool) {
        let toasted = self.toasted;
        until("export_stall", what, 120_000, || {
            app.toasted.get() != toasted || ready()
        })
        .await;
        let total = ms_since(self.started);
        let (worst, busy) = self.beat.stop();
        self.tick.remove();
        let said = match app.toasted.get() == toasted {
            true => None,
            false => toast(app, "Exported ").await.or_else(|| bench_said(app)),
        };
        let pages = PdfDoc::open(out).map_or(0, |d| d.page_count());
        println!(
            "bench export_stall {what} worst_ms={worst:.0} busy_ms={busy:.0} total_ms={total:.0} \
             pages={pages} seen={:?} said={said:?}",
            self.seen.borrow()
        );
        glib::timeout_future(Duration::from_millis(500)).await;
    }
}

/// Every export of a large diagram ([`stall_diagram`], written into the vault): as PDF, PNG and
/// SVG, its Print… as far as its dialog, and its print run to a PDF as GTK prints it; the note
/// `note`, the notebook `notebook` and a note of four of the diagram's pages as HTML; and the
/// diagram's PDF opened as a PDF and exported again; a diagram of one such page as PDF, which has
/// no pages to count; then a PDF export stopped at its fourth page, with the other tabs open. Each
/// is [`Watch`]ed, and written into `dir`, as are pictures of the window while the diagram's PDF
/// counts its pages, alone (`export.png`) and among the other tabs (`tabs.png`).
fn bench_export_stall(app: &Rc<App>, note: &str, notebook: &str, dir: &Path, flat: bool) {
    use crate::diagram::export::{export_to, operation, print};
    use crate::export::{Export, Paper};
    let (rel, embeds, one) = (
        "Export stall.drawio",
        "Export stall.md",
        "Export one.drawio",
    );
    let embedding: String = (0..4)
        .map(|p| format!("Page {p}\n\n![[{rel}#Page {p}]]\n\n"))
        .collect();
    let root = app.root();
    let written = std::fs::write(root.join(rel), stall_diagram(STALL_PAGES, flat))
        .and_then(|()| std::fs::write(root.join(one), stall_diagram(1, flat)))
        .and_then(|()| std::fs::write(root.join(embeds), embedding));
    if let Err(e) = written {
        println!("bench export_stall not_written {e}");
        return bench_quit(app);
    }
    let (app, note, notebook) = (app.clone(), note.to_string(), notebook.to_string());
    let dir = dir.to_path_buf();
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(1500)).await;
        app.open_path(rel);
        let tab = until("export_stall", "diagram tab", 20_000, || {
            app.active_diagram()
        })
        .await;
        let Some(tab) = tab else {
            return bench_quit(&app);
        };
        glib::timeout_future(Duration::from_millis(1000)).await;
        for (to, name) in [
            (Export::Pdf, "stall.pdf"),
            (Export::Png, "stall.png"),
            (Export::Svg, "stall.svg"),
        ] {
            let dest = dir.join(name);
            let _ = std::fs::remove_file(&dest);
            let shot = matches!(to, Export::Pdf).then(|| dir.join("export.png"));
            let watch = Watch::start(&app, shot);
            export_to(&app, &tab, to, dest.clone());
            watch
                .report(&app, &format!("diagram {name}"), &dest, || false)
                .await;
        }

        // Print… until its dialog is up, which is closed unanswered.
        let before = gtk::Window::list_toplevels();
        let dialog = || {
            gtk::Window::list_toplevels()
                .into_iter()
                .find(|w| w.is_visible() && !before.contains(w))
                .and_downcast::<gtk::Window>()
        };
        let watch = Watch::start(&app, None);
        print(&app, &tab);
        watch
            .report(&app, "diagram print_dialog", Path::new(""), || {
                dialog().is_some()
            })
            .await;
        if let Some(dialog) = dialog() {
            dialog.close();
        }
        glib::timeout_future(Duration::from_millis(500)).await;
        // The print itself, as GTK runs it once the dialog is answered: to a PDF here.
        let printed = dir.join("print.pdf");
        let _ = std::fs::remove_file(&printed);
        let watch = Watch::start(&app, None);
        let slices = crate::work::Slices::new();
        let op = operation(&tab.file(), Default::default(), "print", &slices, |_, _| {}).await;
        if let Ok(op) = op {
            op.set_export_filename(&printed);
            let ran = gio::GioFuture::new(&op, |op, _, done| {
                let op = op.clone();
                glib::idle_add_local_once(move || {
                    done.resolve(op.run(gtk::PrintOperationAction::Export, None::<&gtk::Window>))
                });
            });
            let _ = ran.await;
        }
        watch
            .report(&app, "diagram print_run", &printed, || true)
            .await;

        for (rel, name) in [
            (&note, "note.html"),
            (&notebook, "notebook.html"),
            (&embeds.to_string(), "embeds.html"),
        ] {
            app.open_path(rel);
            let paper = until("export_stall", rel, 20_000, || {
                app.doc_for(rel).as_ref().and_then(Paper::of)
            })
            .await;
            let Some(paper) = paper else { continue };
            glib::timeout_future(Duration::from_millis(1000)).await;
            let dest = dir.join(name);
            let _ = std::fs::remove_file(&dest);
            let watch = Watch::start(&app, None);
            app.html_to(paper, dest.clone());
            watch.report(&app, name, &dest, || false).await;
        }

        // The diagram's PDF, opened as a PDF and exported again.
        let copy = "Export stall.pdf";
        if std::fs::copy(dir.join("stall.pdf"), app.root().join(copy)).is_ok()
            && let Some(pdf) = super::pdf::opened(&app, copy).await
        {
            glib::timeout_future(Duration::from_millis(1000)).await;
            let dest = dir.join("stall (exported).pdf");
            let _ = std::fs::remove_file(&dest);
            let watch = Watch::start(&app, None);
            crate::pdf::export::export_to(&app, &pdf, dest.clone());
            watch.report(&app, "pdf export", &dest, || false).await;
        }

        // A diagram of one page, whose line counts nothing and whose bar pulses.
        app.open_path(one);
        let single = || app.active_diagram().filter(|t| t.file().pages.len() == 1);
        if let Some(single) = until("export_stall", "one-page tab", 20_000, single).await {
            glib::timeout_future(Duration::from_millis(1000)).await;
            let dest = dir.join("one.pdf");
            let _ = std::fs::remove_file(&dest);
            let watch = Watch::start(&app, None);
            export_to(&app, &single, Export::Pdf, dest.clone());
            watch.report(&app, "diagram one.pdf", &dest, || false).await;
        }

        // The diagram's PDF again, stopped once its line counts three pages: nothing written, no
        // toast.
        app.open_path(rel);
        glib::timeout_future(Duration::from_millis(1000)).await;
        let dest = dir.join("stopped.pdf");
        let _ = std::fs::remove_file(&dest);
        let watch = Watch::start(&app, Some(dir.join("tabs.png")));
        export_to(&app, &tab, Export::Pdf, dest.clone());
        let counting = || app.statusbar.progress_text().ends_with("3/12 pages");
        until("export_stall", "a page", 60_000, counting).await;
        let stop = app.statusbar.transfer_stop();
        let offered = stop.is_visible();
        stop.emit_clicked();
        let gone = || !app.statusbar.progress_text().contains("stopped.pdf");
        watch.report(&app, "diagram stopped", &dest, gone).await;
        glib::timeout_future(Duration::from_millis(1000)).await;
        println!(
            "bench export_stall stop offered={offered} written={} line={:?} bar={}",
            dest.exists(),
            app.statusbar.progress_text(),
            app.connect.shown()
        );
        println!("bench export_stall done");
        bench_quit(&app);
    });
}
