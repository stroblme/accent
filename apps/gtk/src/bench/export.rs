//! Read back exports and print copies, bypassing file chooser and print-system input.

use super::*;
use accent_core::pdf::{InkStyle, PdfDoc};
use std::path::Path;

/// `pdf:<rel_pdf>`: see [`bench_export_pdf`]; `note:<rel_note>`: see [`bench_export_note`].
pub(super) fn bench_export(app: &Rc<App>, arg: &str) {
    match arg.split_once(':') {
        Some(("pdf", rel)) => bench_export_pdf(app, rel),
        Some(("note", rel)) => bench_export_note(app, rel),
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
        for _ in 0..100 {
            let linked = vault
                .backlinks(&rel)
                .is_ok_and(|links| links.iter().any(|b| b.src_rel_path == note));
            if linked {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        let Some(pdf) = super::pdf::opened(&app, &rel).await else {
            println!("bench export_pdf no_tab");
            return bench_quit(&app);
        };
        for _ in 0..50 {
            if pdf.has_note_links() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
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

/// Export a note as HTML into `$TMPDIR` and read it back: its pictures inlined, its formulas,
/// and nothing of the app or of a script left in it.
fn bench_export_note(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let mut tab = None;
        for _ in 0..100 {
            tab = app.active().filter(|tab| tab.rel() == rel);
            if tab.is_some() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        let Some(tab) = tab else {
            println!("bench export_note no_tab");
            return bench_quit(&app);
        };
        let stem = Path::new(&rel)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let html_path = std::env::temp_dir().join(format!("{stem}.html"));
        let _ = std::fs::remove_file(&html_path);
        app.html_to(tab, html_path.clone());
        let said = toast(&app, &format!("Exported {stem}.html"))
            .await
            .or_else(|| bench_said(&app));
        let html = std::fs::read_to_string(&html_path).unwrap_or_default();
        println!(
            "bench export_html accent_refs={} data_images={} web_images={} svg={} math={} \
             script={} csp={} said={said:?}",
            html.matches("accent:").count(),
            html.matches("src=\"data:").count(),
            html.matches("src=\"https:").count(),
            html.matches("<svg").count(),
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
