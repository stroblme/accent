//! Print… and Export as… for whichever tab the menu acts on, and what their kinds share.
//!
//! A note or a notebook goes out as its HTML page ([`html`]): Export as HTML… writes it, and
//! Print… opens it in the system browser, whose print dialog prints it. PDF and diagram tabs use
//! their own export modules.

use super::*;

pub(crate) mod css;
mod html;

pub(crate) use html::Paper;

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

/// What `doc` can be printed and exported as, `None` where it cannot be: a note, markdown or
/// org, a notebook, a PDF and a diagram are the four (DESIGN.md, Preview, Notebook, PDF and
/// Diagram).
pub fn printable(doc: &Doc) -> Option<Kind> {
    match doc {
        Doc::Text(tab) if tab.flavour().is_note() => Some(Kind::Note),
        Doc::Pdf(_) => Some(Kind::Pdf),
        Doc::Diagram(_) => Some(Kind::Diagram),
        Doc::Notebook(_) => Some(Kind::Notebook),
        _ => None,
    }
}

/// What the file at `key` can be printed and exported as by its name alone, for a file's menu
/// away from its tab, where [`printable`] answers. A marimo `.py` and a diagram saved as `.xml`
/// are known by their bytes, so only their tabs offer it.
pub fn printable_name(key: &str) -> Option<Kind> {
    match doc::kind_of(key) {
        Kind::Text if accent_core::path::is_org(key) => Some(Kind::Note),
        Kind::Text | Kind::Image => None,
        kind => Some(kind),
    }
}

impl App {
    /// Print…: the tab right-clicked, else the one in front.
    pub(crate) fn print(self: &Rc<Self>) {
        if let Some(doc) = self.menu_doc() {
            self.print_doc(&doc);
        }
    }

    /// Print `doc`: a note or a notebook opens in the system browser, which prints it, a PDF and
    /// a diagram go through their tabs' print dialogs; any other tab does nothing, as Save As
    /// does.
    fn print_doc(self: &Rc<Self>, doc: &Doc) {
        match doc {
            Doc::Pdf(pdf) => pdf::export::print(self, pdf),
            Doc::Diagram(tab) => diagram::export::print(self, tab),
            doc => {
                if let Some(paper) = Paper::of(doc) {
                    self.open_in_browser(paper, Some("Opened in your browser. Print from there"))
                }
            }
        }
    }

    /// Print… (`None`) or an export of `key` from a file's menu in the tree or the Git pane, on
    /// its tab: opened, kept, where it is not open yet, and run once it lands.
    pub(crate) fn make(self: &Rc<Self>, key: &str, what: Option<Export>) {
        let run = move |app: &Rc<App>, doc: &Doc| match what {
            None => app.print_doc(doc),
            Some(to) => app.export_doc(doc.clone(), to),
        };
        if let Some(doc) = self.doc_for(key) {
            return run(self, &doc);
        }
        let then = open::Reveal::Then(Box::new(run));
        self.revealing.borrow_mut().insert(key.to_string(), then);
        self.open_path(key);
    }

    /// Open in Browser: the note or notebook the menu acts on, as Print… opens it; any other tab
    /// does nothing.
    pub(crate) fn browse(self: &Rc<Self>) {
        if let Some(paper) = self.menu_doc().as_ref().and_then(Paper::of) {
            self.open_in_browser(paper, None);
        }
    }

    /// Export as…: the tab right-clicked, else the one in front.
    pub(crate) fn export(self: &Rc<Self>, to: Export) {
        if let Some(doc) = self.menu_doc() {
            self.export_doc(doc, to);
        }
    }

    /// Choose a local destination and export `doc` in the supported format.
    /// Notes and notebooks support HTML, PDFs PDF, and diagrams PDF/PNG/SVG; other kinds cannot
    /// export.
    fn export_doc(self: &Rc<Self>, doc: Doc, to: Export) {
        if printable(&doc).is_none() {
            return;
        }
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
            (Export::Html, Doc::Text(_) | Doc::Notebook(_)) => format!("{stem}.html"),
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
                (Doc::Pdf(pdf), _) => pdf::export::export_to(&app, &pdf, dest),
                (Doc::Diagram(tab), to) => diagram::export::export_to(&app, &tab, to, dest),
                (doc, Export::Html) => {
                    if let Some(paper) = Paper::of(&doc) {
                        app.html_to(paper, dest)
                    }
                }
                _ => {}
            }
        });
    }

    /// The document the tab menu's items act on: the tab right-clicked, else the one in front; a
    /// preview tab's note for a preview tab.
    fn menu_doc(&self) -> Option<Doc> {
        let doc = match self.menu_page.borrow().clone() {
            Some(page) => self.doc_for_page(&page),
            None => self.active_doc(),
        };
        doc.and_then(rendered::note_for)
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

/// The locale's paper with [`MARGIN_MM`] all round, which a diagram is printed on.
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
