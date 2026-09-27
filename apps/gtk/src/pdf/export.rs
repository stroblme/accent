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
    let (app, pdf) = (app.clone(), pdf.clone());
    let name = crate::doc::file_name(&pdf.key()).to_string();
    let written = dest.file_name().map(|n| n.to_string_lossy().into_owned());
    glib::spawn_future_local(async move {
        match copied(&pdf, dest).await {
            Ok(()) => app.toast(&format!("Exported {}", written.unwrap_or(name))),
            Err(why) => app.cannot(&format!("export {name}"), why),
        }
    });
}

/// Print…: the copy made in the cache and handed to the print dialog, which sends the file on as
/// it is — vector pages, the print system doing ranges and copies. The copy goes once the dialog
/// is done with it.
pub fn print(app: &Rc<App>, pdf: &Rc<PdfTab>) {
    let (app, pdf) = (app.clone(), pdf.clone());
    let name = crate::doc::file_name(&pdf.key()).to_string();
    glib::spawn_future_local(async move {
        let file = match print_copy(&pdf).await {
            Ok(file) => file,
            Err(why) => return app.cannot(&format!("print {name}"), why),
        };
        let dialog = gtk::PrintDialog::builder()
            .title(name.as_str())
            .modal(true)
            .build();
        let window = app.window.clone();
        dialog.print_file(
            Some(&window),
            None,
            &gio::File::for_path(&file),
            gio::Cancellable::NONE,
            move |result| {
                let _ = std::fs::remove_file(&file);
                // Closing the dialog is an answer, not a failure.
                if let Err(e) = result
                    && !e.matches(gtk::DialogError::Dismissed)
                {
                    app.cannot(&format!("print {name}"), e);
                }
            },
        );
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
