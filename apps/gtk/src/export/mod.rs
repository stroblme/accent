//! Print… and Export as… for whichever tab the menu acts on, and what their kinds share.
//!
//! A note is printed and exported through WebKit ([`note`]), which only a build with the `webkit`
//! feature has; PDF and diagram tabs use their own export modules.

use super::*;

#[cfg(feature = "webkit")]
mod note;

/// What an export writes.
#[derive(Clone, Copy)]
pub enum Export {
    Pdf,
    Html,
    Png,
    Svg,
}

/// A printout's margin on every side, LibreOffice's default: GTK's own quarter inch crowds prose.
const MARGIN_MM: f64 = 20.0;

/// What `doc` can be printed and exported as, `None` where it cannot be: a note, a PDF and a
/// diagram are the three (DESIGN.md, Preview and Diagram), a note only where WebKit can put it
/// on paper.
pub fn printable(doc: &Doc) -> Option<Kind> {
    match doc {
        Doc::Text(tab) if cfg!(feature = "webkit") && tab.flavour().is_note() => Some(Kind::Note),
        Doc::Pdf(_) => Some(Kind::Pdf),
        Doc::Diagram(_) => Some(Kind::Diagram),
        _ => None,
    }
}

impl App {
    /// Print…: the tab right-clicked, else the one in front. A note goes through WebKit's own
    /// print dialog, a PDF and a diagram through their tabs'; any other tab does nothing, as
    /// Save As does.
    pub(crate) fn print(self: &Rc<Self>) {
        match self.menu_doc() {
            #[cfg(feature = "webkit")]
            Some(Doc::Text(tab)) if tab.flavour().is_note() => self.print_note(tab),
            Some(Doc::Pdf(pdf)) => pdf::export::print(self, &pdf),
            Some(Doc::Diagram(tab)) => diagram::export::print(self, &tab),
            _ => {}
        }
    }

    /// Choose a local destination and export the supported format.
    /// Notes support PDF/HTML, PDFs PDF, and diagrams PDF/PNG/SVG; other kinds cannot export.
    pub(crate) fn export(self: &Rc<Self>, to: Export) {
        let Some(doc) = self.menu_doc().filter(|doc| printable(doc).is_some()) else {
            return;
        };
        let key = doc.key();
        let stem = Path::new(doc::file_name(&key))
            .file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        let title = match to {
            Export::Pdf => "Export as PDF",
            Export::Html => "Export as HTML",
            Export::Png => "Export as PNG",
            Export::Svg => "Export as SVG",
        };
        let name = match (to, &doc) {
            (Export::Pdf, Doc::Text(tab)) if tab.flavour().is_note() => format!("{stem}.pdf"),
            (Export::Html, Doc::Text(tab)) if tab.flavour().is_note() => format!("{stem}.html"),
            (Export::Pdf, Doc::Pdf(_)) => format!("{stem} (exported).pdf"),
            (_, Doc::Diagram(tab)) => match diagram::export::offered_name(tab, to) {
                Some(name) => name,
                None => return,
            },
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
                #[cfg(feature = "webkit")]
                (Doc::Text(tab), Export::Pdf) => app.note_pdf_to(tab, dest),
                #[cfg(feature = "webkit")]
                (Doc::Text(tab), Export::Html) => app.note_html_to(tab, dest),
                (Doc::Pdf(pdf), _) => pdf::export::export_to(&app, &pdf, dest),
                (Doc::Diagram(tab), to) => diagram::export::export_to(&app, &tab, to, dest),
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

/// The locale's paper with [`MARGIN_MM`] all round, which a note and a diagram are printed on.
pub(crate) fn page_setup() -> gtk::PageSetup {
    let setup = gtk::PageSetup::new();
    setup.set_top_margin(MARGIN_MM, gtk::Unit::Mm);
    setup.set_bottom_margin(MARGIN_MM, gtk::Unit::Mm);
    setup.set_left_margin(MARGIN_MM, gtk::Unit::Mm);
    setup.set_right_margin(MARGIN_MM, gtk::Unit::Mm);
    setup
}

/// What an export is called in its toasts: the name the chooser was given.
pub(crate) fn dest_name(dest: &Path) -> String {
    dest.file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}
