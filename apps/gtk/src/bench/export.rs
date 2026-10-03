//! Export as PDF and Print, with the file chooser and the print system left out: what each one
//! writes is read back.

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

/// A PDF with a stroke in the file and a note linking two selections on its first page, exported
/// into `$TMPDIR` as Export as PDF… does once its chooser has answered, then onto itself, which
/// is refused. Then Print's copy, and Print itself as far as its dialog, which is closed
/// unanswered. Every copy is read back against the source — pages, highlights and ink — and the
/// source is compared byte for byte with what it was before. The source is written to, so this
/// wants a scratch vault (`make vault VAULT=/tmp/<name>`).
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

/// A note as Export as PDF… and Export as HTML… write it once their choosers have answered, into
/// `$TMPDIR`, each read back: the PDF's head, its pages and whether its first holds the note's
/// first heading, that page drawn beside it as `<stem>.png` for a look; the HTML's leftovers of
/// the app, its inlined images, diagrams, formulas and scripts. Then Print…, as far as its dialog,
/// which is answered Cancel, and whether the busy line went with it.
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
        let heading = tab
            .text()
            .lines()
            .find_map(|l| l.strip_prefix("# "))
            .unwrap_or_default()
            .to_string();
        let dir = std::env::temp_dir();

        let pdf = dir.join(format!("{stem}.pdf"));
        let _ = std::fs::remove_file(&pdf);
        // The vault's own work takes the busy line ahead of an export's, so it goes first.
        for _ in 0..300 {
            if app.statusbar.progress_text().is_empty() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        let started = Instant::now();
        app.note_pdf_to(tab.clone(), pdf.clone());
        // Timed to the busy line going, which is the export's end: its toast may wait behind
        // another one.
        let busy = app.statusbar.progress_text();
        while app.statusbar.progress_text() == busy {
            glib::timeout_future(Duration::from_millis(10)).await;
        }
        let ms = ms_since(started);
        let said = toast(&app, &format!("Exported {stem}.pdf"))
            .await
            .or_else(|| bench_said(&app));
        let head = std::fs::read(&pdf).is_ok_and(|b| b.starts_with(b"%PDF-"));
        let (pages, on_first) = match PdfDoc::open(&pdf) {
            Ok(doc) => {
                let text: String = doc
                    .page_text(0)
                    .unwrap_or_default()
                    .iter()
                    .map(|g| g.ch)
                    .collect();
                if let Ok(image) = doc.render_page(0, 1.0, accent_core::pdf::Theme::Plain) {
                    let texture = gdk::MemoryTexture::new(
                        image.width as i32,
                        image.height as i32,
                        gdk::MemoryFormat::R8g8b8a8,
                        &glib::Bytes::from_owned(image.data),
                        image.width as usize * 4,
                    );
                    let _ = texture.save_to_png(dir.join(format!("{stem}.png")));
                }
                (doc.page_count(), text.contains(&heading))
            }
            Err(_) => (0, false),
        };
        println!(
            "bench export_note_pdf head={head} pages={pages} heading={on_first} ms={ms:.0} \
             busy={busy:?} said={said:?}"
        );

        let html_path = dir.join(format!("{stem}.html"));
        let _ = std::fs::remove_file(&html_path);
        app.note_html_to(tab.clone(), html_path.clone());
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

        let before = gtk::Window::list_toplevels();
        app.print();
        let mut dialog = None;
        for _ in 0..100 {
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
        // Answered as Cancel is, the way WebKit hears a dialog end: closing it under WebKit
        // would have it destroy a window already gone.
        if let Some(dialog) = dialog {
            use glib::translate::IntoGlib;
            dialog.emit_by_name::<()>("response", &[&gtk::ResponseType::Cancel.into_glib()]);
        }
        glib::timeout_future(Duration::from_millis(500)).await;
        println!(
            "bench print_note dialog={kind:?} busy={busy:?} busy_after={:?}",
            app.statusbar.progress_text()
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
    doc.add_ink(0, &[(60.0, 200.0), (200.0, 240.0)], pen)?;
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
