//! Export as PDF and Print: the document with its ink and the notes' highlights, written as a
//! copy — the file itself never changes — and for Print that copy handed to the print system,
//! so what is printed is what the tab shows.

use std::path::PathBuf;
use std::rc::Rc;

use gtk::{gio, glib};

use super::tab::PdfTab;
use crate::App;

/// Export as PDF… once the destination is chosen: the copy written to `dest`, and a toast.
pub fn export_to(app: &Rc<App>, pdf: &Rc<PdfTab>, dest: PathBuf) {
    let name = dest
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let (pdf, done) = (pdf.clone(), format!("Exported {name}"));
    busy_with(
        app,
        format!("Exporting {name}…"),
        format!("export {name}"),
        async move { copied(&pdf, dest).await.map(|()| Some(done)) },
    );
}

/// Print…: the copy made in the cache and handed to the print dialog, which sends the file on as
/// it is — vector pages, the print system doing ranges and copies. The copy goes once the dialog
/// is done with it.
pub fn print(app: &Rc<App>, pdf: &Rc<PdfTab>) {
    let name = crate::doc::file_name(&pdf.key()).to_string();
    let (window, pdf, title) = (app.window.clone(), pdf.clone(), name.clone());
    busy_with(
        app,
        format!("Printing {name}…"),
        format!("print {name}"),
        async move {
            let file = print_copy(&pdf).await?;
            let dialog = gtk::PrintDialog::builder()
                .title(title.as_str())
                .modal(true)
                .build();
            let printed = dialog
                .print_file_future(Some(&window), None, &gio::File::for_path(&file))
                .await;
            let _ = std::fs::remove_file(&file);
            match printed {
                // Closing the dialog is an answer, not a failure.
                Err(e) if !e.matches(gtk::DialogError::Dismissed) => Err(e.to_string()),
                _ => Ok(None),
            }
        },
    );
}

/// Run `work` while the status bar says `busy`, then say how it went: the toast it asks for, or
/// that the window cannot `what`, with its reason — as a note's Print and Export say it.
fn busy_with(
    app: &Rc<App>,
    busy: String,
    what: String,
    work: impl Future<Output = Result<Option<String>, String>> + 'static,
) {
    app.statusbar.set_transfer(&busy, true);
    let app = app.clone();
    glib::spawn_future_local(async move {
        let said = work.await;
        app.statusbar.set_transfer(&busy, false);
        match said {
            Ok(Some(done)) => app.toast(&done),
            Ok(None) => {}
            Err(why) => app.cannot(&what, why),
        }
    });
}

/// Print's copy, in the cache under the document's own name.
pub(crate) async fn print_copy(pdf: &PdfTab) -> Result<PathBuf, String> {
    let dir = glib::user_cache_dir().join("accent").join("print");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join(crate::doc::file_name(&pdf.key()));
    copied(pdf, file.clone()).await.map(|()| file)
}

/// [`PdfTab::copy_to`] in the accent, waited for off the main loop.
async fn copied(pdf: &PdfTab, dest: PathBuf) -> Result<(), String> {
    let answer = pdf.copy_to(dest, crate::theme::accent_rgb());
    match crate::work::off_thread("pdf copy", move || answer.recv()).await {
        Some(Ok(done)) => done,
        _ => Err("the document is not open".to_string()),
    }
}
