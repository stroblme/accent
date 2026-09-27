//! Print… and Export as PDF… or HTML… (DESIGN.md, Preview).
//!
//! A note goes as its tab holds it, unsaved edits and all, into a preview of its own that is never
//! shown ([`Preview::for_paper`]): white paper whatever the theme, its images as their files are,
//! its diagrams drawn. Once the page is in, WebKit paginates it — for its own print dialog, or for
//! GTK's Print to File printer, which writes the PDF — or it is read back as one self-contained
//! HTML file. A PDF tab's print and export are `pdf::export`'s.

use super::*;
use crate::preview::{self, Preview};
use std::sync::Mutex;
use webkit6::prelude::*;

/// What an export writes.
#[derive(Clone, Copy)]
pub enum Export {
    Pdf,
    Html,
}

/// Waited on before the page is taken: the diagrams the mermaid bootstrap draws, then the fonts.
/// The images are in by then, a page's load waiting for them.
const READY: &str = "await window.__accentDrawn; await document.fonts.ready; return true;";

/// How long a page may take to be ready, so a web process that never answers ends in a toast
/// rather than a busy line that never goes.
const PATIENCE: Duration = Duration::from_secs(30);

/// A printout's margin on every side, LibreOffice's default: GTK's own quarter inch crowds prose.
const MARGIN_MM: f64 = 20.0;

/// The page as one file. Every image's address is made absolute, so the export can fetch it; a
/// link into the vault, which means nothing outside the app, is left as its text, a same-page
/// `#` anchor being kept; the note's own scripts and the scroll-sync markers are taken out.
const TAKE_HTML: &str = r#"
document.querySelectorAll('img[src]').forEach(function (img) { img.setAttribute('src', img.src); });
document.querySelectorAll('a[href]').forEach(function (a) {
  if (a.href.startsWith('accent:') && !a.getAttribute('href').startsWith('#')) {
    a.replaceWith(...a.childNodes);
  }
});
document.querySelectorAll('script, span[data-line]').forEach(function (e) { e.remove(); });
return '<!DOCTYPE html>' + document.documentElement.outerHTML;
"#;

/// What `doc` can be printed and exported as, `None` where it cannot be: a note and a PDF are the
/// two (DESIGN.md, Preview).
pub fn printable(doc: &Doc) -> Option<Kind> {
    match doc {
        Doc::Text(tab) if tab.flavour().is_note() => Some(Kind::Note),
        Doc::Pdf(_) => Some(Kind::Pdf),
        _ => None,
    }
}

impl App {
    /// Print…: the tab right-clicked, else the one in front. A note goes through WebKit's own
    /// print dialog, a PDF through its tab's; any other tab does nothing, as Save As does.
    pub(crate) fn print(self: &Rc<Self>) {
        match self.menu_doc() {
            Some(Doc::Text(tab)) if tab.flavour().is_note() => self.print_note(tab),
            Some(Doc::Pdf(pdf)) => pdf::export::print(self, &pdf),
            _ => {}
        }
    }

    /// Export as PDF… and Export as HTML…: the destination asked for on this machine, starting in
    /// the file's own folder on a local vault, then the note or the PDF written there. A PDF has
    /// no HTML, and any other tab has neither.
    pub(crate) fn export(self: &Rc<Self>, to: Export) {
        let Some(doc) = self.menu_doc() else { return };
        let key = doc.key();
        let stem = Path::new(doc::file_name(&key))
            .file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        let (title, name) = match (to, printable(&doc)) {
            (Export::Pdf, Some(Kind::Note)) => ("Export as PDF", format!("{stem}.pdf")),
            (Export::Html, Some(Kind::Note)) => ("Export as HTML", format!("{stem}.html")),
            (Export::Pdf, Some(Kind::Pdf)) => ("Export as PDF", format!("{stem} (exported).pdf")),
            _ => return,
        };
        let dialog = gtk::FileDialog::builder()
            .title(title)
            .initial_name(&name)
            .modal(true)
            .build();
        // A host's folders are not on this machine, so there the chooser starts where it likes.
        if !self.on_host(&key)
            && let Some(dir) = self.root().join(&key).parent()
        {
            dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
        }
        let app = Rc::downgrade(self);
        dialog.save(Some(&self.window), gio::Cancellable::NONE, move |result| {
            // The error is almost always "the user closed the chooser", which needs no toast.
            let (Some(app), Some(dest)) = (app.upgrade(), result.ok().and_then(|f| f.path()))
            else {
                return;
            };
            match (doc, to) {
                (Doc::Text(tab), Export::Pdf) => app.note_pdf_to(tab, dest),
                (Doc::Text(tab), Export::Html) => app.note_html_to(tab, dest),
                (Doc::Pdf(pdf), _) => pdf::export::export_to(&app, &pdf, dest),
                _ => {}
            }
        });
    }

    /// The document the tab menu's items act on: the tab right-clicked, else the one in front.
    fn menu_doc(&self) -> Option<Doc> {
        match self.menu_page.borrow().clone() {
            Some(page) => self.doc_for_page(&page),
            None => self.active_doc(),
        }
    }

    /// Print… for a note: the note on paper through WebKit's print dialog, which paginates it for
    /// the paper chosen there.
    fn print_note(self: &Rc<Self>, tab: Rc<Tab>) {
        let name = doc::file_name(&tab.rel()).to_string();
        let app = self.clone();
        self.busy_with(
            format!("Printing {name}…"),
            format!("print {name}"),
            async move {
                let page = paper(&app, &tab).await?;
                let window = app.window.clone();
                printed(&operation(&page), move |op| {
                    Ok(op.run_dialog(Some(&window)) == webkit6::PrintOperationResponse::Print)
                })
                .await?;
                Ok(None)
            },
        );
    }

    /// Export as PDF… for a note, once the chooser has answered: the note on paper, printed into
    /// `dest` by GTK's Print to File printer on the locale's paper.
    pub(crate) fn note_pdf_to(self: &Rc<Self>, tab: Rc<Tab>, dest: PathBuf) {
        let name = dest_name(&dest);
        let app = self.clone();
        self.busy_with(
            format!("Exporting {name}…"),
            format!("export {name}"),
            async move {
                let page = paper(&app, &tab).await?;
                printed(&operation(&page), move |op| {
                    let printer = file_printer().ok_or("there is no Print to File printer")?;
                    let settings = gtk::PrintSettings::new();
                    settings.set_printer(&printer);
                    settings.set(gtk::PRINT_SETTINGS_OUTPUT_FILE_FORMAT, Some("pdf"));
                    let uri = gio::File::for_path(&dest).uri();
                    settings.set(gtk::PRINT_SETTINGS_OUTPUT_URI, Some(&uri));
                    op.set_print_settings(&settings);
                    op.print();
                    Ok(true)
                })
                .await?;
                Ok(Some(format!("Exported {name}")))
            },
        );
    }

    /// Export as HTML… for a note, once the chooser has answered: the page read back, its images
    /// fetched into it and the preview's sheet put in its head, written to `dest`.
    pub(crate) fn note_html_to(self: &Rc<Self>, tab: Rc<Tab>, dest: PathBuf) {
        let name = dest_name(&dest);
        let title = Path::new(doc::file_name(&tab.rel()))
            .file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        let app = self.clone();
        self.busy_with(
            format!("Exporting {name}…"),
            format!("export {name}"),
            async move {
                let page = paper(&app, &tab).await?;
                let html = page
                    .view()
                    .call_async_javascript_function_future(TAKE_HTML, None, None, None)
                    .await
                    .map_err(|e| e.to_string())?
                    .to_str()
                    .to_string();
                let (resolve, css) = (app.asset_resolver(), preview::paper_css());
                // Fetching an image is a round trip on a remote vault, and the page may hold many.
                let write = move || {
                    let body =
                        inline_images(&html, |uri| data_uri(&preview::asset(&*resolve, uri)?));
                    accent_core::fs::write_bytes(
                        &dest,
                        standalone(&title, &css, &body).as_bytes(),
                        None,
                    )
                };
                match crate::work::off_thread("export", write).await {
                    Some(Ok(_)) => Ok(Some(format!("Exported {name}"))),
                    Some(Err(e)) => Err(e.to_string()),
                    None => Err("the worker stopped".to_string()),
                }
            },
        );
    }

    /// Run `work` while the status bar says `busy`, then say how it went: the toast it asks for,
    /// or that the window cannot `what`, with its reason. A PDF's print and export say it so too.
    pub(crate) fn busy_with(
        self: &Rc<Self>,
        busy: String,
        what: String,
        work: impl Future<Output = Result<Option<String>, String>> + 'static,
    ) {
        self.statusbar.set_transfer(&busy, true);
        let app = self.clone();
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
}

/// What `tab` holds on paper, ready to be taken: loaded, its diagrams drawn, its fonts in.
async fn paper(app: &Rc<App>, tab: &Rc<Tab>) -> Result<Preview, String> {
    let page = Preview::for_paper(app.asset_resolver());
    let view = page.view().clone();
    // Connected when first polled, below, in this turn of the main loop: before the load the
    // render starts can have finished.
    let loaded = gio::GioFuture::new(&view, |view, _, done| {
        let done = Cell::new(Some(done));
        view.connect_load_changed(move |_, event| {
            if event == webkit6::LoadEvent::Finished
                && let Some(done) = done.take()
            {
                done.resolve(());
            }
        });
    });
    page.render(&tab.rel(), &tab.text());
    let ready = async {
        loaded.await;
        view.call_async_javascript_function_future(READY, None, None, None)
            .await
    };
    match glib::future_with_timeout(PATIENCE, ready).await {
        Ok(Ok(_)) => Ok(page),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("the page took too long to lay out".to_string()),
    }
}

/// A print of `page` on the locale's paper, [`MARGIN_MM`] all round.
fn operation(page: &Preview) -> webkit6::PrintOperation {
    let setup = gtk::PageSetup::new();
    setup.set_top_margin(MARGIN_MM, gtk::Unit::Mm);
    setup.set_bottom_margin(MARGIN_MM, gtk::Unit::Mm);
    setup.set_left_margin(MARGIN_MM, gtk::Unit::Mm);
    setup.set_right_margin(MARGIN_MM, gtk::Unit::Mm);
    let op = webkit6::PrintOperation::new(page.view());
    op.set_page_setup(&setup);
    op
}

/// Start `op` with `start`, which says whether it did, and wait until it is over: whether it
/// printed, `false` for a print never started (its dialog cancelled). The page is the caller's to
/// hold meanwhile: WebKit reads it as it prints.
///
/// `start` runs from an idle: the print dialog and the printer lookup each run a main loop of
/// their own, and glib aborts on a future polled inside another's poll, which that loop would do.
async fn printed(
    op: &webkit6::PrintOperation,
    start: impl FnOnce(&webkit6::PrintOperation) -> Result<bool, String> + 'static,
) -> Result<bool, String> {
    gio::GioFuture::new(op, move |op, _, done| {
        let done = Rc::new(Cell::new(Some(done)));
        let failed: Rc<RefCell<Option<glib::Error>>> = Rc::default();
        op.connect_failed(glib::clone!(
            #[strong]
            failed,
            move |_, e| {
                failed.replace(Some(e.clone()));
            }
        ));
        // WebKit says `failed` before `finished`, which comes either way.
        op.connect_finished(glib::clone!(
            #[strong]
            done,
            move |_| {
                if let Some(done) = done.take() {
                    done.resolve(failed.take().map_or(Ok(true), |e| Err(e.to_string())));
                }
            }
        ));
        let op = op.clone();
        glib::idle_add_local_once(move || match start(&op) {
            Ok(true) => {}
            outcome => {
                if let Some(done) = done.take() {
                    done.resolve(outcome);
                }
            }
        });
    })
    .await
}

/// The name of GTK's Print to File printer, which is how WebKit is pointed at it, and which is
/// translated: so it is found by its backend, the one printer the file backend offers. GTK 4.22
/// builds that backend in as `GtkPrintBackendFileBuiltin`; a GTK loading it as a module calls it
/// `GtkPrintBackendFile`.
fn file_printer() -> Option<String> {
    let found = Arc::new(Mutex::new(None));
    let slot = found.clone();
    gtk::enumerate_printers(
        move |printer| {
            let backend = printer.property::<glib::Object>("backend");
            let file = backend.type_().name().starts_with("GtkPrintBackendFile");
            if file && let Ok(mut slot) = slot.lock() {
                *slot = Some(printer.name().to_string());
            }
            // Stops the enumeration.
            file
        },
        true,
    );
    found.lock().ok()?.take()
}

/// What an export is called in its toasts: the name the chooser was given.
fn dest_name(dest: &Path) -> String {
    dest.file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

/// `path` as a `data:` URI, typed by its name and its bytes.
fn data_uri(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let (kind, _) = gio::content_type_guess(Some(path), Some(bytes.as_slice()));
    let mime = gio::content_type_get_mime_type(&kind)?;
    Some(format!(
        "data:{mime};base64,{}",
        glib::base64_encode(&bytes)
    ))
}

/// Every `accent:` image source in `html` put inside it by `fetch`, as a `data:` URI; one it
/// cannot fetch loses its `src` rather than pointing at an address nothing outside the app can
/// open. Any other source, a web image's, stays as it is.
fn inline_images(html: &str, fetch: impl Fn(&str) -> Option<String>) -> String {
    const SRC: &str = " src=\"";
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find(SRC) {
        let from = at + SRC.len();
        let Some(len) = rest[from..].find('"') else {
            break;
        };
        let (value, after) = (&rest[from..from + len], from + len + 1);
        out.push_str(&rest[..at]);
        match value.starts_with("accent:") {
            // The attribute is as `outerHTML` escapes it.
            true => {
                if let Some(data) = fetch(&value.replace("&amp;", "&")) {
                    out.push_str(&format!("{SRC}{data}\""));
                }
            }
            false => out.push_str(&rest[at..after]),
        }
        rest = &rest[after..];
    }
    out.push_str(rest);
    out
}

/// `html` as a file of its own: its title, a policy that runs no script whatever the note holds,
/// and the preview's sheet, which WebKit applied from outside the page's markup.
fn standalone(title: &str, css: &str, html: &str) -> String {
    let head = format!(
        "<meta http-equiv=\"Content-Security-Policy\" content=\"script-src 'none'\">\
         <title>{}</title><style>{css}</style></head>",
        glib::markup_escape_text(title)
    );
    html.replacen("</head>", &head, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exported_page_carries_its_images_and_nothing_of_the_app() {
        let page = "<!DOCTYPE html><html><head><meta charset=\"utf-8\"></head><body>\
            <img src=\"accent://file/a%20b.png?x=1&amp;y=2\">\
            <img src=\"https://example.org/x.png\">\
            <img alt=\"gone\" src=\"accent://file/gone.png\"></body></html>";
        let body = inline_images(page, |uri| {
            (uri == "accent://file/a%20b.png?x=1&y=2").then(|| "data:image/png;base64,AA==".into())
        });
        let file = standalone("Note & Co", "p { margin: 0; }", &body);
        assert!(!file.contains("accent:"), "{file}");
        assert!(
            file.contains("<img src=\"data:image/png;base64,AA==\">"),
            "{file}"
        );
        assert!(
            file.contains("<img src=\"https://example.org/x.png\">"),
            "{file}"
        );
        assert!(file.contains("<img alt=\"gone\">"), "{file}");
        assert!(file.contains("content=\"script-src 'none'\""), "{file}");
        assert!(file.contains("<title>Note &amp; Co</title>"), "{file}");
        assert!(
            file.contains("<style>p { margin: 0; }</style></head>"),
            "{file}"
        );
    }
}
